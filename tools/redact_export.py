#!/usr/bin/env python3
"""Type-skeleton redactor for a Snapchat "My Data" export.

Turns a real export into a fixture set that has the same SHAPE and none of the
content, so a parser can be tested against it without anyone reading the data.

READS
  every *.json anywhere under SRC, plus the file NAMES inside the listing dirs
  (--listing-dir, default: chat_media, memories). Media files are never opened.

WRITES
  DST/<masked relative path>.json   the redacted mirror
  DST/listings/<dir>.json           filename patterns, counts, masked examples
  DST/_redaction_report.json        settings, per-file true array lengths, tallies

PRINTS
  file names, integers, type/class tallies, and payload-free pattern strings.
  Never a value taken from the input.

HOW IT DECIDES
  A redacted value may only contain bytes this run GENERATED, or bytes from a
  CLOSED vocabulary compiled into this file (the enum words, file extensions,
  filename and path words, coordinate labels, known URL hosts and known URL
  parameter names -- all listed as frozensets above the code). Anything outside
  that vocabulary is synthesized, never echoed: an unknown URL host, an unknown
  parameter name, free text where a coordinate label was expected. The
  self-check then re-reads the output and fails on any token it cannot trace to
  one of those two sources, so a synthesizer that starts echoing its input
  cannot vouch for itself.

GUARANTEES
  * Every leaf value is replaced by a synthetic token of the same JSON type.
    Booleans are re-rolled; null and "" are kept as-is.
  * Array length is preserved up to --array-sample elements; the true length of
    every truncated array is recorded in the report.
  * Dates keep their exact format, moved by one run-wide offset.
  * Coordinates are fake, drawn from the [1.0, 2.0] band next to null island,
    keeping the original decimal count and dropping the sign. Numeric
    latitude/longitude are recognised by KEY NAME (lat, lon, coordinates, ...).
  * A UUID becomes the all-zero UUID, unless it sits in a join field.
  * JOIN FIELDS (a username, handle, conversation id or message id, in key or
    value position) are synthesized from the VALUE, keyed with --seed, so the
    same real value maps to the same handle (`user_1`, `conv_2`) everywhere and
    cross-file matching can be tested. This deliberately preserves WHICH
    entries share a value. The value itself is not recoverable from the handle,
    and the seed is never written to the output.
  * Top-level keys of the files in DEFAULT_KEY_MASK_PATHS (chat_history.json,
    snap_history.json, talk_history.json) are masked by default, because those
    files key their data BY USERNAME. A key that is a known schema label is kept
    verbatim so parser tests still work; anything else becomes a handle. Opt out
    per file with --keep-keys-in.
  * File and dir NAMES are masked with the same vocabulary rule, so
    `chat_with_<user>.json` mirrors as `chat_with_xxxx.json` while
    `memories_history.json` keeps its name. Every name this tool prints, reports
    or matches --mask-keys-under against is that MIRRORED name, and every pointer
    segment it prints or records is masked unless it is an array index, a handle,
    or a known schema label. That holds on the abort paths too (unreadable file,
    invalid json, duplicate key): they name the MIRRORED file and mask the
    duplicate key. Since no fixture exists after an abort, --show-source-names
    unmasks those three messages so the file can actually be repaired.
  * A self-check re-reads everything written and exits 3 unless all of:
      - no alnum run of --max-alnum-run chars or longer, anywhere;
      - no URL carrying userinfo, a live path segment, or a parameter payload;
      - every coordinate pair in a value inside the fake band;
      - every >=6-char token in a mirror value is generated or vocabulary;
      - every number in a mirror value is one this run generated.

NOT GUARANTEED
  * The vocabularies and the default-masked file list are BEST EFFORT: they were
    written without access to a real export. A file that is not on the list
    keeps its keys, and a schema label this file does not know becomes a handle.
    Read them, then re-run with --mask-keys-under / --keep-keys-in as needed.
  * Dict keys outside a masked container pass through verbatim, because the
    schema is the point. The tool prints an advisory with a pasteable rule
    whenever a container looks keyed by id.
  * Recorded true array lengths are exact real counts (a per-conversation
    message count is real data), and so are the join-handle total in the report
    (roughly a friend count), emptiness, nullness and, for a number outside a
    coordinate key, its sign and decade.
  * The date offset is one constant for the whole run, so intervals between
    dates survive exactly. The offset is not written to the output, but anyone
    who learns one real date can recover it and invert every other date.
  * A float pair that is not under a coordinate-ish key keeps its sign and
    decade, so it can still LOOK like a coordinate to a human auditor.

Exit codes: 0 clean, 1 bad configuration or unreadable input, 2 argparse usage
error, 3 self-check failure.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import random
import re
import secrets
import sys
from collections import Counter
from dataclasses import dataclass, field
from datetime import datetime, timedelta
from fnmatch import fnmatch
from pathlib import Path
from typing import Any, Callable, Iterator, Sequence
from urllib.parse import urlsplit, urlunsplit

TOOL = "redact_export.py"
REPORT_NAME = "_redaction_report.json"
LISTINGS_DIRNAME = "listings"

REDACTED = "REDACTED"
ZERO_UUID = "00000000-0000-0000-0000-000000000000"
MASK_CHAR = "*"
NAME_MASK_CHAR = "x"
DYNAMIC_KEY_PLACEHOLDER = "<key>"
SYNTH_HOST_SUFFIX = ".invalid"  # RFC 2606: can never resolve

DEFAULT_ARRAY_SAMPLE = 25
DEFAULT_EXAMPLES = 3
DEFAULT_LISTING_DIRS = ("chat_media", "memories")

# The synthesizers cap their own output at 16 chars (epoch ints) and 12 chars
# (masked name words), so a limit under 20 would fail on clean data. The ceiling
# exists so nobody can silently disable the rule with a huge number.
MIN_ALNUM_RUN = 20
DEFAULT_MAX_ALNUM_RUN = 20
MAX_ALNUM_RUN_CEILING = 64
MAX_SYNTH_WORD = 12

# Token granularity for the accounting rule. Shorter runs are dominated by
# years, "UTC" and single digits, which carry no content on their own.
MIN_TOKEN = 6

FAKE_COORD_LO = 1.0
FAKE_COORD_HI = 2.0

# A container with at least this many same-shaped values reads like a map keyed
# by id rather than a schema object. Advisory only; keys are never auto-masked
# outside DEFAULT_KEY_MASK_PATHS.
DYNAMIC_KEY_HINT = 4

TAG_ALPHABET = "abcdefghijklmnopqrstuvwxyz0123456789"

# --------------------------------------------------------------------------
# closed vocabularies: the ONLY input-derived bytes a value may keep.
# Everything here is best effort, written without a real export in hand.
# --------------------------------------------------------------------------

# Type discriminators a parser branches on. Case-sensitive, values only.
ENUM_VALUES = frozenset(
    {
        "PHOTO",
        "VIDEO",
        "VIDEO_NO_SOUND",
        "IMAGE",
        "AUDIO",
        "AUDIO_NOTE",
        "TEXT",
        "MEDIA",
        "STICKER",
        "NOTE",
        "SNAP",
        "STATUS",
        "LOCATION",
        "SHARE",
        "GIF",
        "BITMOJI",
        "SENT",
        "RECEIVED",
        "DELIVERED",
        "VIEWED",
        "SCREENSHOT",
        "SAVED",
        "UNSAVED",
        "DELETED",
        "PENDING",
        "EXPIRED",
        "OPENED",
        "REPLAYED",
        "TRUE",
        "FALSE",
        "NONE",
        "NULL",
        "UNKNOWN",
        "OTHER",
        "PUBLIC",
        "PRIVATE",
        "FRIENDS",
        "EVERYONE",
        "CUSTOM",
    }
)

# An all-caps enum word is also a plausible nickname, so the allowlist is not
# applied under a key that names a person or carries free text.
FREE_TEXT_KEY_RE = re.compile(
    r"(name|user|display|nick|title|caption|content|text|message|subject|note|bio|"
    r"description|comment|address|email|phone)"
)

FILE_EXTENSIONS = frozenset(
    {
        ".jpg",
        ".jpeg",
        ".png",
        ".webp",
        ".gif",
        ".heic",
        ".heif",
        ".mp4",
        ".mov",
        ".m4v",
        ".avi",
        ".webm",
        ".mp3",
        ".m4a",
        ".aac",
        ".ogg",
        ".opus",
        ".wav",
        ".json",
        ".txt",
        ".html",
        ".csv",
        ".zip",
        ".vcf",
    }
)

# Words that name a role or a schema file, never an owner. Used for filenames,
# for mirrored path segments, and for the listing patterns.
NAME_VOCABULARY = frozenset(
    {
        # media roles
        "media",
        "overlay",
        "overlays",
        "thumbnail",
        "thumbnails",
        "metadata",
        "main",
        "zip",
        "part",
        "mydata",
        "image",
        "video",
        "audio",
        "note",
        "sticker",
        # export file words
        "account",
        "history",
        "chat",
        "snap",
        "snaps",
        "story",
        "stories",
        "memories",
        "friends",
        "location",
        "ranking",
        "shared",
        "subscriptions",
        "support",
        "talk",
        "terms",
        "user",
        "profile",
        "index",
        "community",
        "lenses",
        "connected",
        "apps",
        "purchase",
        "app",
        "in",
        "with",
        "json",
        "html",
        "files",
        "data",
        "bitmoji",
        "custom",
        "email",
        "emails",
        "campaign",
        "feature",
        "ads",
        "pro",
        "snapchat",
        "ai",
        "plus",
    }
)

# The only labels that may precede a coordinate pair. Anything else in that
# position is free text (a place or person label) and gets synthesized.
LATLON_LABELS = frozenset(
    {
        "latitude, longitude:",
        "latitude,longitude:",
        "lat, lon:",
        "lat,lon:",
        "lat, long:",
        "lat,long:",
        "latitude:",
        "longitude:",
        "location:",
        "coordinates:",
        "coordinate:",
    }
)

# A URL keeps its host only if the host is one of these. Any other host is
# synthesized, so a personal or unexpected host cannot ride along.
KNOWN_URL_HOSTS = frozenset(
    {
        "app.snapchat.com",
        "web.snapchat.com",
        "accounts.snapchat.com",
        "snapchat.com",
        "www.snapchat.com",
        "cf-st.sc-cdn.net",
        "bolt-gcdn.sc-cdn.net",
        "ms.sc-cdn.net",
        "sc-cdn.net",
        "story.snapchat.com",
    }
)

# A query parameter keeps its NAME only if the name is one of these.
KNOWN_URL_PARAMS = frozenset(
    {
        "uid",
        "sid",
        "mid",
        "cid",
        "sig",
        "signature",
        "policy",
        "key",
        "token",
        "hash",
        "type",
        "mo",
        "ttl",
        "expires",
        "expiry",
        "t",
        "e",
        "v",
        "id",
        "dmd",
        "media",
        "format",
    }
)

# Known schema labels. A key inside a default-masked container survives only if
# it is one of these; anything else becomes a handle.
SCHEMA_KEYS = frozenset(
    {
        "received saved chat history",
        "sent saved chat history",
        "received chat history",
        "sent chat history",
        "received snap history",
        "sent snap history",
        "saved media",
        "saved chat history",
        "chat history",
        "snap history",
        "talk history",
        "conversations",
        "messages",
        "basic information",
        "device information",
        "frequent locations",
        "latest location",
        "friends",
        "deleted friends",
        "blocked users",
        "hidden friend suggestions",
        "your subscriptions",
        "your stories",
        "shared stories",
    }
)

# Files whose TOP-LEVEL keys are data, not schema: they key by username.
DEFAULT_KEY_MASK_PATHS = ("chat_history.json", "snap_history.json", "talk_history.json")

# Keys whose value joins records across files. Normalised: lowercase, no spaces,
# underscores or hyphens.
JOIN_KEY_CLASSES = (
    (
        "user",
        frozenset(
            {
                "username",
                "user",
                "userid",
                "from",
                "to",
                "sender",
                "recipient",
                "recipients",
                "participant",
                "participants",
                "friend",
                "handle",
                "screenname",
                "displayname",
                "addedby",
            }
        ),
    ),
    (
        "conv",
        frozenset(
            {
                "conversationid",
                "conversationtitle",
                "conversation",
                "chatid",
                "groupid",
                "threadid",
            }
        ),
    ),
    (
        "id",
        frozenset(
            {"id", "mediaid", "messageid", "snapid", "memoryid", "externalid", "uuid", "guid"}
        ),
    ),
)

# Tokens the tool itself writes into a value.
STATIC_ACCOUNTED = frozenset(
    {"redacted", "invalid"}
    | {word for word in NAME_VOCABULARY if len(word) >= MIN_TOKEN}
    | {value.lower() for value in ENUM_VALUES if len(value) >= MIN_TOKEN}
    | {ext[1:].lower() for ext in FILE_EXTENSIONS if len(ext) - 1 >= MIN_TOKEN}
    | {
        token.lower()
        for source in (LATLON_LABELS | KNOWN_URL_HOSTS | KNOWN_URL_PARAMS)
        for token in re.findall(r"[A-Za-z0-9]+", source)
        if len(token) >= MIN_TOKEN
    }
)

UUID_RE = re.compile(
    r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}"
)
ALNUM_RE = re.compile(r"[A-Za-z0-9]+")
# Masked and zeroed runs the tool emits: x-runs for name payload, 0-runs for the
# all-zero uuid and an unparseable date.
MASK_RUN_RE = re.compile(r"x+|0+")
URL_START_RE = re.compile(r"^[A-Za-z][A-Za-z0-9+.\-]*://")
URL_CANDIDATE_RE = re.compile(r"[A-Za-z][A-Za-z0-9+.\-]*://[^\s\"'\\<>]+")
COORD_PAIR_RE = re.compile(r"([-+]?\d{1,3}\.\d+)\s*,\s*([-+]?\d{1,3}\.\d+)")
LATLON_RE = re.compile(
    r"^(?P<prefix>[A-Za-z ,]*:\s*|)"
    r"(?P<lat>[-+]?\d{1,3}\.\d+)\s*,\s*(?P<lon>[-+]?\d{1,3}\.\d+)$"
)
COORD_KEY_RE = re.compile(
    r"^(lat|latitude|lon|lng|long|longitude|coord|coords|coordinate|coordinates|latlon|latlng|geo)$"
)

# Datetime shapes are rebuilt field-by-field over the original string, so every
# separator and suffix survives byte-for-byte without a format table. Only the
# suffixes named here can be echoed, and none of them is 6 chars or longer.
DATETIME_PATTERNS = (
    re.compile(
        r"^(?P<Y>\d{4})-(?P<m>\d{2})-(?P<d>\d{2})[ T]"
        r"(?P<H>\d{2}):(?P<M>\d{2}):(?P<S>\d{2})"
        r"(?P<frac>\.\d{1,9})?"
        r"(?:\s*(?:UTC|GMT|Z|[+-]\d{2}:?\d{2}))?$"
    ),
    re.compile(r"^(?P<Y>\d{4})-(?P<m>\d{2})-(?P<d>\d{2})$"),
    re.compile(r"^(?P<Y>\d{4})/(?P<m>\d{2})/(?P<d>\d{2})$"),
    re.compile(r"^(?P<m>\d{2})/(?P<d>\d{2})/(?P<Y>\d{4})$"),
)

NAME_TOKEN_RE = re.compile(
    r"(?P<uuid>[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})"
    r"|(?P<date>\d{4}-\d{2}-\d{2})"
    r"|(?P<word>[A-Za-z][A-Za-z0-9]*)"
    r"|(?P<digits>\d+)"
    # Only ascii punctuation and space echo verbatim. Everything else non-alnum
    # (unicode, control chars) is payload: a display name can live there.
    r"|(?P<sep>[ -/:-@\[-`{-~]+)"
    r"|(?P<other>[^A-Za-z0-9]+)"
)

# Digit count -> ticks per second, for spotting a timestamp integer.
EPOCH_UNITS = ((10, 1), (13, 10**3), (16, 10**6), (19, 10**9))
EPOCH_LO_SECONDS = 1.0e9
EPOCH_HI_SECONDS = 2.1e9

EXIT_CONFIG = 1
EXIT_SELF_CHECK = 3

RULE_ALNUM = "no alnum run >= --max-alnum-run"
RULE_URL = "no url carrying userinfo, a path segment, or a parameter payload"
RULE_COORD = f"every coordinate pair in a value inside the fake [{FAKE_COORD_LO}, {FAKE_COORD_HI}] band"
RULE_VALUES = f"every >= {MIN_TOKEN}-char token in a mirror value is generated or vocabulary"
RULE_NUMBERS = "every number in a mirror value is one this run generated"
SELF_CHECK_RULES = (RULE_ALNUM, RULE_URL, RULE_COORD, RULE_VALUES, RULE_NUMBERS)


class RedactError(Exception):
    """A user-fixable problem: bad arguments, unreadable input, unsafe paths."""


@dataclass
class Stats:
    json_files: int = 0
    leaf_types: Counter = field(default_factory=Counter)
    value_classes: Counter = field(default_factory=Counter)
    truncated_arrays: int = 0
    masked_keys: int = 0
    kept_schema_keys: int = 0
    renamed_paths: list[str] = field(default_factory=list)
    keys: set[str] = field(default_factory=set)
    # How many containers each key name appears in. A schema label repeats across
    # sibling records; a username used AS a key appears exactly once.
    key_containers: Counter = field(default_factory=Counter)
    # Tokens and numbers this run GENERATED. Nothing input-derived goes in here,
    # so a path that echoes its input cannot vouch for itself.
    generated: set[str] = field(default_factory=set)
    generated_numbers: set[Any] = field(default_factory=set)
    # Closed-vocabulary members actually emitted, for the report's audit trail.
    vocabulary_used: set[str] = field(default_factory=set)
    dynamic_containers: list[dict] = field(default_factory=list)

    def note_generated(self, text: str) -> None:
        for token in ALNUM_RE.findall(text):
            if len(token) >= MIN_TOKEN:
                self.generated.add(token.lower())

    def note_number(self, value: Any) -> None:
        self.generated_numbers.add(value)

    def note_vocabulary(self, member: str) -> None:
        self.vocabulary_used.add(member)


class Handles:
    """Value-keyed handles for join fields: the same real value always maps to
    the same handle, so cross-file matching survives. The map is keyed by a
    seeded digest, so the handle cannot be turned back into the value."""

    def __init__(self, seed: int) -> None:
        self.key = str(seed).encode("utf-8")
        self.table: dict[bytes, str] = {}
        self.counters: Counter = Counter()

    def digest(self, value: str) -> bytes:
        return hashlib.blake2b(value.encode("utf-8"), key=self.key, digest_size=16).digest()

    def handle(self, value: str, prefix: str) -> str:
        digest = self.digest(value)
        known = self.table.get(digest)
        if known is not None:
            return known
        self.counters[prefix] += 1
        assigned = f"{prefix}_{self.counters[prefix]}"
        self.table[digest] = assigned
        return assigned

    def uuid(self, value: str) -> str:
        hexed = self.digest(value).hex()
        return "-".join(
            (hexed[0:8], hexed[8:12], hexed[12:16], hexed[16:20], hexed[20:32])
        )

    def number(self, value: str, digits: int) -> int:
        drawn = int.from_bytes(self.digest(value)[:8], "big")
        if digits <= 1:
            return drawn % 10
        return 10 ** (digits - 1) + drawn % (10**digits - 10 ** (digits - 1))


@dataclass
class MaskRule:
    glob: str
    segments: tuple
    raw: str
    builtin: bool = False
    hits: int = 0


@dataclass
class Ctx:
    seed: int
    shift: timedelta
    array_sample: int
    max_alnum_run: int
    mask_rules: Sequence[MaskRule]
    stats: Stats
    handles: Handles
    show_source_names: bool = False
    # Both mirrored names of the file being walked; see matching_rules.
    rel_aliases: tuple = ()
    truncations: list[dict] = field(default_factory=list)

    @property
    def shift_seconds(self) -> int:
        return int(self.shift.total_seconds())

    def rng(self, *parts: object) -> random.Random:
        return random.Random(seed_int(self.seed, *parts))


def seed_int(*parts: object) -> int:
    joined = "\x00".join(str(part) for part in parts)
    digest = hashlib.blake2b(joined.encode("utf-8"), digest_size=8).digest()
    return int.from_bytes(digest, "big")


# --------------------------------------------------------------------------
# small helpers
# --------------------------------------------------------------------------


def strip_prefix(text: str, prefix: str) -> str:
    return text[len(prefix) :] if text.startswith(prefix) else text


def decimals_of(text: str) -> int | None:
    """Digits after the decimal point, or None for exponent/integer forms."""
    if "e" in text or "E" in text or "." not in text:
        return None
    return len(text.split(".", 1)[1])


def split_known_ext(name: str) -> tuple[str, str]:
    stem, ext = os.path.splitext(name)
    if ext.lower() in FILE_EXTENSIONS:
        return stem, ext
    return name, ""


def longest_alnum_run(text: str) -> int:
    return max((len(m.group()) for m in ALNUM_RE.finditer(text)), default=0)


def normalize_key(key: str) -> str:
    return re.sub(r"[\s_\-]+", "", key).lower()


def normalize_label(label: str) -> str:
    return " ".join(label.lower().split())


def mask_text(text: str) -> str:
    """Star every word run, keeping punctuation and the literal REDACTED. Used
    for everything the tool prints about a violation: a privacy tool's failure
    path must not become its widest leak."""
    return re.sub(
        r"\w+",
        lambda m: m.group() if m.group() == REDACTED else MASK_CHAR * len(m.group()),
        text,
    )


def mask_excerpt(text: str, limit: int = 80) -> str:
    masked = mask_text(text)
    return masked if len(masked) <= limit else masked[:limit] + "..."


def mask_pointer(pointer: str) -> str:
    """Star every named segment, keep array indices: an index locates the leak
    without naming anyone."""
    return "/".join(
        segment
        if segment.isdigit() or segment == DYNAMIC_KEY_PLACEHOLDER
        else mask_text(segment)
        for segment in pointer.split("/")
    )


def pointer_escape(key: str) -> str:
    return key.replace("~", "~0").replace("/", "~1")


def json_pointer(path: str) -> str:
    return path or "/"


def json_kind(value: Any) -> str:
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "bool"
    if isinstance(value, int):
        return "int"
    if isinstance(value, float):
        return "float"
    if isinstance(value, str):
        return "str"
    if isinstance(value, list):
        return "list"
    return "dict"


def names_free_text(key: str | None) -> bool:
    """A person or free-text key, EXCEPT one that names a type or a state: those
    carry the enum a parser branches on."""
    if key is None:
        return False
    normalized = normalize_key(key)
    if normalized.endswith(("type", "status", "state", "kind")):
        return False
    return FREE_TEXT_KEY_RE.search(normalized) is not None


def join_class(key: str | None) -> str | None:
    if key is None:
        return None
    normalized = normalize_key(key)
    for prefix, names in JOIN_KEY_CLASSES:
        if normalized in names:
            return prefix
    return None


def is_coord_key(key: str | None) -> bool:
    if key is None:
        return False
    return COORD_KEY_RE.fullmatch(normalize_key(key)) is not None


# --------------------------------------------------------------------------
# name tokenizer: shared by listings, mirrored paths, and filename values
# --------------------------------------------------------------------------


def tokenize_name(stem: str) -> list[tuple[str, str]]:
    out: list[tuple[str, str]] = []
    for match in NAME_TOKEN_RE.finditer(stem):
        kind = match.lastgroup or "sep"
        text = match.group()
        if kind == "word" and text.lower() in NAME_VOCABULARY:
            kind = "literal"
        out.append((kind, text))
    return out


def render_pattern(tokens: Sequence[tuple[str, str]]) -> str:
    parts = []
    for kind, text in tokens:
        if kind == "uuid":
            parts.append("<uuid>")
        elif kind == "date":
            parts.append("YYYY-MM-DD")
        elif kind == "digits":
            parts.append(f"<d:{len(text)}>")
        elif kind == "word":
            parts.append(f"<w:{len(text)}>")
        elif kind == "other":
            parts.append(f"<u:{len(text)}>")
        else:
            parts.append(text)
    return "".join(parts)


def render_masked(tokens: Sequence[tuple[str, str]]) -> str:
    parts = []
    for kind, text in tokens:
        if kind in ("literal", "sep"):
            parts.append(text)
        elif kind == "other":
            parts.append(MASK_CHAR * len(text))
        else:
            parts.append("".join(MASK_CHAR if ch.isalnum() else ch for ch in text))
    return "".join(parts)


def mask_name(name: str) -> str:
    """A path segment keeps only vocabulary words, separators and a known
    extension; every other run becomes a mask run."""
    stem, ext = split_known_ext(name)
    parts: list[str] = []
    for kind, text in tokenize_name(stem):
        if kind in ("literal", "sep"):
            parts.append(text)
            continue
        piece = NAME_MASK_CHAR * min(len(text), MAX_SYNTH_WORD)
        if parts and parts[-1][-1:].isalnum() and piece[:1].isalnum():
            parts.append("-")
        parts.append(piece)
    return "".join(parts) + ext


def mask_relative_path(rel: str) -> str:
    return "/".join(mask_name(part) for part in rel.split("/"))


# --------------------------------------------------------------------------
# format-preserving synthesis
# --------------------------------------------------------------------------


def shift_date_only(text: str, shift: timedelta) -> str:
    try:
        moved = datetime.strptime(text, "%Y-%m-%d") + shift
    except (ValueError, OverflowError):
        return "0000-00-00"
    return moved.strftime("%Y-%m-%d")


def synth_datetime(value: str, ctx: Ctx, path: str) -> str | None:
    for pattern in DATETIME_PATTERNS:
        match = pattern.match(value)
        if match is None:
            continue
        groups = match.groupdict()
        try:
            base = datetime(
                int(groups["Y"]),
                int(groups["m"]),
                int(groups["d"]),
                int(groups.get("H") or 0),
                int(groups.get("M") or 0),
                int(groups.get("S") or 0),
            )
            moved = base + ctx.shift
        except (ValueError, OverflowError):
            return None  # shaped like a date but not one; let the generic path have it
        chars = list(value)
        # Every field is zero-padded to a fixed width, so span lengths never
        # change and the remaining spans stay valid while substituting.
        for name, width, number in (
            ("Y", 4, moved.year),
            ("m", 2, moved.month),
            ("d", 2, moved.day),
            ("H", 2, moved.hour),
            ("M", 2, moved.minute),
            ("S", 2, moved.second),
        ):
            if groups.get(name) is None:
                continue
            start, end = match.span(name)
            chars[start:end] = f"{number:0{width}d}"
        if groups.get("frac"):
            rng = ctx.rng(path, "frac")
            start, end = match.span("frac")
            digits = "".join(rng.choice("0123456789") for _ in range(end - start - 1))
            chars[start:end] = "." + digits
            ctx.stats.note_generated(digits)
        return "".join(chars)
    return None


def fake_coord(rng: random.Random, decimals: int, stats: Stats) -> str:
    out = f"{rng.uniform(FAKE_COORD_LO, FAKE_COORD_HI):.{decimals}f}"
    stats.note_generated(out)
    return out


def synth_latlon(value: str, ctx: Ctx, path: str) -> str | None:
    match = LATLON_RE.match(value)
    if match is None:
        return None
    prefix = match.group("prefix")
    if prefix:
        label = normalize_label(prefix)
        if label not in LATLON_LABELS:
            return None  # free text in label position; destroy the whole string
        ctx.stats.note_vocabulary(label)
    rng = ctx.rng(path, "latlon")
    lat = fake_coord(rng, len(match.group("lat").split(".", 1)[1]), ctx.stats)
    lon = fake_coord(rng, len(match.group("lon").split(".", 1)[1]), ctx.stats)
    return (
        value[: match.start("lat")]
        + lat
        + value[match.end("lat") : match.start("lon")]
        + lon
        + value[match.end("lon") :]
    )


def synth_url(value: str, ctx: Ctx, path: str) -> str | None:
    if URL_START_RE.match(value) is None:
        return None
    try:
        parts = urlsplit(value)
    except ValueError:
        return None  # not parseable as a url; the generic path destroys it instead
    host = parts.netloc.rpartition("@")[2].lower()  # userinfo can be a credential
    if host in KNOWN_URL_HOSTS:
        ctx.stats.note_vocabulary(host)
    else:
        host = synth_generic(ctx, path, "host") + SYNTH_HOST_SUFFIX
    redacted_path = "/".join(REDACTED if seg else seg for seg in parts.path.split("/"))
    chunks = []
    for chunk in parts.query.split("&"):
        if not chunk:
            continue
        name = chunk.partition("=")[0].lower()
        if "=" in chunk and name in KNOWN_URL_PARAMS:
            ctx.stats.note_vocabulary(name)
            chunks.append(f"{name}={REDACTED}")
        else:
            chunks.append(REDACTED)  # unknown name, or no name=value shape at all
    scheme = parts.scheme if parts.scheme in ("http", "https") else "https"
    fragment = REDACTED if parts.fragment else ""
    return urlunsplit((scheme, host, redacted_path, "&".join(chunks), fragment))


def synth_uuid(value: str, ctx: Ctx, path: str) -> str | None:
    return ZERO_UUID if UUID_RE.fullmatch(value) else None


def synth_name_piece(kind: str, text: str, ctx: Ctx) -> str:
    """Every piece is regenerated, zeroed, or a run of the mask char, so a
    filename needs no accounting exception: payload never survives as itself."""
    if kind == "uuid":
        return ZERO_UUID
    if kind == "date":
        return shift_date_only(text, ctx.shift)
    if kind == "literal":
        ctx.stats.note_vocabulary(text.lower())
        return text
    return NAME_MASK_CHAR * min(len(text), MAX_SYNTH_WORD)


def synth_filename(value: str, ctx: Ctx, path: str) -> str | None:
    stem, ext = split_known_ext(value)
    if not ext:
        return None
    parts: list[str] = []
    for kind, text in tokenize_name(stem):
        if kind == "sep":
            parts.append(text)
            continue
        piece = synth_name_piece(kind, text, ctx)
        # A separator between two alnum pieces stops them merging into one long
        # token, which the self-check could then not account for piece by piece.
        if parts and parts[-1][-1:].isalnum() and piece[:1].isalnum():
            parts.append("-")
        parts.append(piece)
    out = "".join(parts) + ext
    if longest_alnum_run(out) >= ctx.max_alnum_run:
        return synth_generic(ctx, path) + ext  # pathological stem: drop the shape
    return out


def synth_extension_word(value: str, ctx: Ctx, path: str) -> str | None:
    candidate = "." + strip_prefix(value, ".").lower()
    if candidate in FILE_EXTENSIONS and len(value) <= 5:
        ctx.stats.note_vocabulary(candidate)
        return value
    return None


def synth_digit_string(value: str, ctx: Ctx, path: str) -> str | None:
    if not value.isdigit():
        return None
    shifted = shift_epoch(int(value), ctx.shift_seconds)
    if shifted is not None and len(str(shifted)) == len(value):
        ctx.stats.note_generated(str(shifted))
        return str(shifted)
    rng = ctx.rng(path, "digitstr")
    digits = "".join(
        rng.choice("0123456789") for _ in range(min(len(value), ctx.max_alnum_run - 1))
    )
    ctx.stats.note_generated(digits)
    return digits


def synth_generic(ctx: Ctx, path: str, purpose: str = "str") -> str:
    rng = ctx.rng(path, purpose)
    tag = "".join(rng.choice(TAG_ALPHABET) for _ in range(6))
    ctx.stats.note_generated(tag)
    return "redacted-" + tag


def synth_join(value: str, ctx: Ctx, prefix: str) -> str:
    """Value-keyed, so the same real value maps to the same handle everywhere."""
    out = ctx.handles.uuid(value) if UUID_RE.fullmatch(value) else ctx.handles.handle(value, prefix)
    # Past 100k distinct values a handle's counter reaches 6 digits, which is a
    # token the accounting rule has to be able to trace.
    ctx.stats.note_generated(out)
    return out


def shift_epoch(value: int, shift_seconds: int) -> int | None:
    if value <= 0:
        return None
    digits = len(str(value))
    for width, unit in EPOCH_UNITS:
        if digits != width:
            continue
        if EPOCH_LO_SECONDS <= value / unit < EPOCH_HI_SECONDS:
            return value + shift_seconds * unit
    return None


def synth_int(value: int, ctx: Ctx, path: str, key: str | None) -> int:
    prefix = join_class(key)
    if prefix is not None:
        ctx.stats.value_classes["join int"] += 1
        out = ctx.handles.number(str(value), len(str(abs(value))))
        ctx.stats.note_number(out)
        return out
    shifted = shift_epoch(value, ctx.shift_seconds)
    if shifted is not None:
        ctx.stats.value_classes["epoch int"] += 1
        ctx.stats.note_number(shifted)
        return shifted
    ctx.stats.value_classes["int"] += 1
    rng = ctx.rng(path, "int")
    # Ceiling: a wider synthetic int would trip the tool's own alnum rule.
    digits = min(len(str(abs(value))), ctx.max_alnum_run - 1)
    drawn = rng.randrange(0, 10) if digits <= 1 else rng.randrange(10 ** (digits - 1), 10**digits)
    out = -drawn if value < 0 else drawn
    ctx.stats.note_number(out)
    return out


def synth_float(value: float, ctx: Ctx, path: str, key: str | None) -> float:
    rng = ctx.rng(path, "float")
    text = repr(value)
    decimals = decimals_of(text)
    if is_coord_key(key):
        ctx.stats.value_classes["coord float"] += 1
        out = round(rng.uniform(FAKE_COORD_LO, FAKE_COORD_HI), decimals or 6)
        ctx.stats.note_number(out)
        return out
    ctx.stats.value_classes["float"] += 1
    if decimals is None:
        out = round(rng.uniform(0.0, 1.0), 6)
    else:
        whole = strip_prefix(text, "-").split(".", 1)[0]
        digits = len(whole)
        low = 0.0 if whole == "0" else float(10 ** (digits - 1))
        drawn = round(rng.uniform(low, float(10**digits)), decimals)
        out = -drawn if value < 0 else drawn
    ctx.stats.note_number(out)
    return out


SYNTH_CHAIN: tuple[tuple[str, Callable], ...] = (
    ("url", synth_url),
    ("latlon", synth_latlon),
    ("datetime", synth_datetime),
    ("uuid", synth_uuid),
    ("extension", synth_extension_word),
    ("filename", synth_filename),
    ("digit string", synth_digit_string),
)


def has_own_format(value: str) -> bool:
    """A date, coordinate or url under a join-ish key (`From`, `To`) is a range
    bound, not an identity: keeping its format keeps that parser path covered."""
    return (
        URL_START_RE.match(value) is not None
        or LATLON_RE.match(value) is not None
        or any(pattern.match(value) for pattern in DATETIME_PATTERNS)
    )


def synth_string(value: str, ctx: Ctx, path: str, key: str | None) -> str:
    if value == "":
        ctx.stats.value_classes["empty (kept)"] += 1
        return ""
    prefix = join_class(key)
    if prefix is not None and not has_own_format(value):
        ctx.stats.value_classes["join handle"] += 1
        return synth_join(value, ctx, prefix)
    if value in ENUM_VALUES and not names_free_text(key):
        ctx.stats.value_classes["enum (kept)"] += 1
        ctx.stats.note_vocabulary(value)
        return value
    for name, synth in SYNTH_CHAIN:
        out = synth(value, ctx, path)
        if out is not None:
            ctx.stats.value_classes[name] += 1
            return out
    ctx.stats.value_classes["generic string"] += 1
    return synth_generic(ctx, path)


# --------------------------------------------------------------------------
# the walk
# --------------------------------------------------------------------------


def parse_mask_rule(raw: str, builtin: bool = False) -> MaskRule:
    if raw.startswith("/"):
        glob, pointer = "*", raw
    elif ":" in raw:
        glob, pointer = raw.split(":", 1)
    else:
        raise RedactError(
            f"--mask-keys-under {raw!r}: expected '/json/pointer' or 'FILE:/json/pointer' "
            "(use 'chat_history.json:/' for a file's top-level keys, and '*' as a "
            "segment to match every array index or key at that level)"
        )
    if not pointer.startswith("/"):
        raise RedactError(f"--mask-keys-under {raw!r}: the pointer must start with '/'")
    normalized = "" if pointer == "/" else pointer
    return MaskRule(glob=glob, segments=tuple(normalized.split("/")), raw=raw, builtin=builtin)


def builtin_mask_rules(keep_keys_in: Sequence[str]) -> list[MaskRule]:
    kept = {name.strip() for name in keep_keys_in}
    return [
        parse_mask_rule(f"{name}:/", builtin=True)
        for name in DEFAULT_KEY_MASK_PATHS
        if name not in kept
    ]


def matching_rules(rules: Sequence[MaskRule], names: Sequence[str], path: str) -> list[MaskRule]:
    """Every rule that covers this container, so a rule duplicating a built-in
    still counts as used. `names` holds both mirrored names of the file: the
    shared pre-dedup name, which covers every mirror that collided on it, and
    the deduped name, which resolves to this mirror alone. Matching on both is
    what stops one rule from masking the first colliding mirror and silently
    leaving the rest with real keys."""
    candidates = {name for rel in names for name in (rel, os.path.basename(rel))}
    segments = path.split("/")
    matches = []
    for rule in rules:
        if len(rule.segments) != len(segments):
            continue
        if not any(fnmatch(candidate, rule.glob) for candidate in candidates):
            continue
        if all(want in ("*", have) for want, have in zip(rule.segments, segments)):
            matches.append(rule)
    return matches


def redact_node(
    node: Any, ctx: Ctx, path: str, rel: str, key: str | None = None, parent: str | None = None
) -> Any:
    if isinstance(node, dict):
        return redact_dict(node, ctx, path, rel, key)
    if isinstance(node, list):
        return redact_list(node, ctx, path, rel, key, parent)
    return redact_leaf(node, ctx, path, key, parent)


def redact_dict(node: dict, ctx: Ctx, path: str, rel: str, parent: str | None = None) -> dict:
    matches = matching_rules(ctx.mask_rules, ctx.rel_aliases or (rel,), path)
    for matched in matches:
        matched.hits += 1
    # An explicit rule beats a built-in one: the user asked for every key.
    rule = next((match for match in matches if not match.builtin), None) or (
        matches[0] if matches else None
    )
    ctx.stats.key_containers.update(set(node))
    if (
        rule is None
        and len(node) >= DYNAMIC_KEY_HINT
        and len({json_kind(v) for v in node.values()}) == 1
    ):
        ctx.stats.dynamic_containers.append(
            {
                "file": rel,
                "pointer": path,
                "keys": len(node),
                "key_names": tuple(node),
                "values": json_kind(next(iter(node.values()))),
            }
        )
    out = {}
    for key, value in node.items():
        name = key
        if rule is not None:
            # A built-in rule is a heuristic, so a known schema label survives it;
            # an explicit rule is the user's call and masks everything.
            if rule.builtin and normalize_label(key) in SCHEMA_KEYS:
                ctx.stats.kept_schema_keys += 1
                ctx.stats.note_vocabulary(normalize_label(key))
            else:
                name = synth_join(key, ctx, "user" if rule.builtin else "key")
                ctx.stats.masked_keys += 1
        if name == key:
            ctx.stats.keys.add(key)
        out[name] = redact_node(value, ctx, f"{path}/{pointer_escape(name)}", rel, key, parent)
    return out


def redact_list(
    node: list, ctx: Ctx, path: str, rel: str, key: str | None, parent: str | None = None
) -> list:
    kept = node
    if ctx.array_sample > 0 and len(node) > ctx.array_sample:
        kept = node[: ctx.array_sample]
        ctx.truncations.append({"pointer": path, "true_length": len(node), "kept": len(kept)})
        ctx.stats.truncated_arrays += 1
    return [
        redact_node(item, ctx, f"{path}/{index}", rel, key, parent)
        for index, item in enumerate(kept)
    ]


def redact_leaf(value: Any, ctx: Ctx, path: str, key: str | None, parent: str | None) -> Any:
    stats = ctx.stats
    if value is None:
        stats.leaf_types["null"] += 1
        return None
    if isinstance(value, bool):  # before int: bool is an int subclass
        stats.leaf_types["bool"] += 1
        return ctx.rng(path, "bool").choice((False, True))
    if isinstance(value, int):
        stats.leaf_types["int"] += 1
        return synth_int(value, ctx, path, key)
    if isinstance(value, float):
        stats.leaf_types["float"] += 1
        # A coordinate can sit under a coord-ish PARENT (`{"geo": {"x": ...}}`),
        # so the parent stands in when the leaf's own key says nothing.
        return synth_float(
            value, ctx, path, parent if not is_coord_key(key) and is_coord_key(parent) else key
        )
    if isinstance(value, str):
        stats.leaf_types["str"] += 1
        return synth_string(value, ctx, path, key)
    raise RedactError(
        f"unsupported json leaf type {type(value).__name__} at "
        f"{mask_pointer(json_pointer(path))}: the input is not plain json"
    )


def duplicate_key_guard(name: str, show_key: bool) -> Callable:
    def hook(pairs):
        seen = set()
        for key, _ in pairs:
            if key in seen:
                shown = key if show_key else mask_excerpt(key)
                raise RedactError(
                    f"{name} has a duplicate json key ({shown}): the mirror would "
                    "silently lose one of them, so de-duplicate the file, then re-run"
                )
            seen.add(key)
        return dict(pairs)

    return hook


def redact_file(src_file: Path, source_rel: str, aliases: tuple, ctx: Ctx) -> tuple[Any, list]:
    """Abort messages name the MIRRORED file like every other channel, unless
    --show-source-names is on: the reason for masking (the user may paste this
    output to an assistant) outlives the abort."""
    shown = source_rel if ctx.show_source_names else aliases[0]
    hint = "" if ctx.show_source_names else " (pass --show-source-names to see which source file)"
    try:
        raw = src_file.read_text(encoding="utf-8")
    except UnicodeDecodeError as exc:
        raise RedactError(
            f"cannot read {shown}: it is not utf-8 ({exc.reason} at byte {exc.start}); "
            f"exclude the file, then re-run{hint}"
        ) from exc
    except OSError as exc:
        # str(OSError) carries the absolute source path, which would undo the masking
        # of `shown` two tokens earlier — interpolate fields, never the exception.
        raise RedactError(f"cannot read {shown}: {exc.strerror} (errno {exc.errno}){hint}") from exc
    try:
        data = json.loads(
            raw, object_pairs_hook=duplicate_key_guard(shown, ctx.show_source_names)
        )
    except json.JSONDecodeError as exc:
        raise RedactError(
            f"{shown} is not valid json ({exc.msg} at line {exc.lineno}): "
            f"exclude or repair the file, then re-run{hint}"
        ) from exc
    ctx.truncations = []
    ctx.stats.json_files += 1
    return redact_node(data, ctx, "", aliases[0]), ctx.truncations


# --------------------------------------------------------------------------
# listings
# --------------------------------------------------------------------------


def build_listing(dir_path: Path, name: str, ctx: Ctx, examples: int) -> dict:
    files = 0
    subdirs = 0
    patterns: dict[str, dict] = {}
    for path in sorted(dir_path.rglob("*")):
        if path.is_dir():
            subdirs += 1
            continue
        if not path.is_file():
            continue
        files += 1
        stem, ext = split_known_ext(path.name)
        tokens = tokenize_name(stem)
        pattern = render_pattern(tokens) + ext
        entry = patterns.setdefault(pattern, {"pattern": pattern, "count": 0, "examples": []})
        entry["count"] += 1
        if len(entry["examples"]) < examples:
            entry["examples"].append(render_masked(tokens) + ext)
    ordered = sorted(patterns.values(), key=lambda e: (-e["count"], e["pattern"]))
    # The name comes from --listing-dir, but it names a real directory in the
    # export, so it is export content and gets the same vocabulary rule.
    return {
        "dir": mask_name(name),
        "file_count": files,
        "subdir_count": subdirs,
        "patterns": ordered,
    }


# --------------------------------------------------------------------------
# self-check
# --------------------------------------------------------------------------


def iter_leaves(node: Any, path: str = "") -> Iterator[tuple[str, Any]]:
    if isinstance(node, dict):
        for key, value in node.items():
            yield from iter_leaves(value, f"{path}/{pointer_escape(str(key))}")
    elif isinstance(node, list):
        for index, item in enumerate(node):
            yield from iter_leaves(item, f"{path}/{index}")
    else:
        yield json_pointer(path), node


def url_problem(url: str) -> str | None:
    try:
        parts = urlsplit(url.rstrip(".,);"))
    except ValueError:
        return "an unparseable url"
    if "@" in parts.netloc:
        return "userinfo"
    for segment in parts.path.split("/"):
        if segment and segment != REDACTED:
            return "a path segment"
    for chunk in parts.query.split("&"):
        if not chunk or chunk == REDACTED:
            continue
        name, sep, value = chunk.partition("=")
        if not sep or value != REDACTED or name.lower() not in KNOWN_URL_PARAMS:
            return "a parameter payload"
    if parts.fragment and parts.fragment != REDACTED:
        return "a fragment"
    return None


def coord_in_band(text: str) -> bool:
    try:
        value = float(text)
    except ValueError:
        return False
    return FAKE_COORD_LO - 1e-9 <= value <= FAKE_COORD_HI + 1e-9


def token_accounted(token: str, stats: Stats) -> bool:
    low = token.lower()
    return (
        low in STATIC_ACCOUNTED
        or low in stats.generated
        or MASK_RUN_RE.fullmatch(low) is not None
    )


def self_check(
    dst: Path, stats: Stats, mirror_rels: Sequence[str], max_alnum_run: int
) -> list[str]:
    """Re-read everything written; return one PII-free description per violation."""
    failures: list[str] = []
    run_re = re.compile(r"[A-Za-z0-9]{%d,}" % max_alnum_run)
    mirrors = set(mirror_rels)
    listings_dir = dst / LISTINGS_DIRNAME
    listings = (
        {f"{LISTINGS_DIRNAME}/{path.name}" for path in listings_dir.iterdir()}
        if listings_dir.is_dir()
        else set()
    )
    for path in sorted(dst.rglob("*")):
        if not path.is_file():
            continue
        rel = path.relative_to(dst).as_posix()
        text = path.read_text(encoding="utf-8")
        for match in run_re.finditer(text):
            failures.append(
                f"{rel}: {RULE_ALNUM} -- {len(match.group())} chars "
                f"({mask_excerpt(match.group())})"
            )
        for match in URL_CANDIDATE_RE.finditer(text):
            problem = url_problem(match.group())
            if problem is not None:
                failures.append(
                    f"{rel}: {RULE_URL} -- carries {problem} ({mask_excerpt(match.group())})"
                )
        if rel not in mirrors and rel not in listings:
            continue  # the report is tool-authored prose; the rules above still cover it
        for pointer, value in iter_leaves(json.loads(text)):
            where = f"{rel} at {mask_pointer(pointer)}"
            if isinstance(value, str):
                for match in COORD_PAIR_RE.finditer(value):
                    if not (coord_in_band(match.group(1)) and coord_in_band(match.group(2))):
                        failures.append(
                            f"{where}: {RULE_COORD} -- ({mask_excerpt(match.group())})"
                        )
                for token in ALNUM_RE.findall(value):
                    if len(token) >= MIN_TOKEN and not token_accounted(token, stats):
                        failures.append(
                            f"{where}: {RULE_VALUES} -- a {len(token)}-char token "
                            f"({MASK_CHAR * len(token)})"
                        )
            elif rel in mirrors and not isinstance(value, bool) and isinstance(value, (int, float)):
                if value not in stats.generated_numbers:
                    failures.append(
                        f"{where}: {RULE_NUMBERS} -- a {len(str(abs(value)))}-char number"
                    )
    return failures


# --------------------------------------------------------------------------
# cli
# --------------------------------------------------------------------------


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog=TOOL,
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("src", type=Path, help="the export dir to read (never modified)")
    parser.add_argument("dst", type=Path, help="where the redacted mirror is written")
    parser.add_argument(
        "--array-sample",
        type=int,
        default=DEFAULT_ARRAY_SAMPLE,
        metavar="N",
        help="keep the first N elements of longer arrays; 0 keeps every element "
        f"(default: {DEFAULT_ARRAY_SAMPLE})",
    )
    parser.add_argument(
        "--examples",
        type=int,
        default=DEFAULT_EXAMPLES,
        metavar="N",
        help=f"masked example filenames per pattern (default: {DEFAULT_EXAMPLES})",
    )
    parser.add_argument(
        "--listing-dir",
        action="append",
        default=None,
        metavar="NAME",
        help="dir name to classify by filename pattern, repeatable "
        f"(default: {', '.join(DEFAULT_LISTING_DIRS)})",
    )
    parser.add_argument(
        "--mask-keys-under",
        action="append",
        default=[],
        metavar="PTR",
        help="also replace the direct child keys of this container with handles, as "
        "'FILE:/json/pointer' or '/json/pointer', with '*' matching any single "
        "segment; repeatable. FILE is the MIRRORED name, the one this tool prints: the "
        "shared name covers every mirror that collided on it, a '-N' name resolves to that "
        "one mirror. A rule that matches nothing is an error, never a silent no-op",
    )
    parser.add_argument(
        "--keep-keys-in",
        action="append",
        default=[],
        metavar="FILE",
        help="turn OFF the built-in key masking for one file, repeatable "
        f"(built in: {', '.join(DEFAULT_KEY_MASK_PATHS)})",
    )
    parser.add_argument(
        "--date-shift-days",
        type=int,
        default=None,
        metavar="N",
        help="shift every date by exactly N days (default: a nonzero offset "
        "derived from --seed and never written to the output)",
    )
    parser.add_argument(
        "--seed",
        type=int,
        default=None,
        metavar="N",
        help="make the run reproducible; the default is random per run and is "
        "never recorded",
    )
    parser.add_argument(
        "--max-alnum-run",
        type=int,
        default=DEFAULT_MAX_ALNUM_RUN,
        metavar="N",
        help=f"self-check limit on alnum run length (default: {DEFAULT_MAX_ALNUM_RUN}, "
        f"allowed: {MIN_ALNUM_RUN}..{MAX_ALNUM_RUN_CEILING})",
    )
    parser.add_argument(
        "--show-source-names",
        action="store_true",
        help="on an abort (unreadable file, invalid json, duplicate key) print the real "
        "SOURCE path and the offending key instead of the masked mirror name, so you can "
        "find the file. DO NOT paste that output to an assistant or into a bug report: it "
        "is the one thing this tool otherwise never prints",
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="write into a non-empty destination",
    )
    return parser


def validate(args: argparse.Namespace) -> None:
    if not args.src.is_dir():
        raise RedactError(f"src {args.src} is not a directory: point it at the export root")
    if args.array_sample < 0:
        raise RedactError(f"--array-sample {args.array_sample}: must be 0 or more")
    if args.examples < 0:
        raise RedactError(f"--examples {args.examples}: must be 0 or more")
    if not MIN_ALNUM_RUN <= args.max_alnum_run <= MAX_ALNUM_RUN_CEILING:
        raise RedactError(
            f"--max-alnum-run {args.max_alnum_run}: must be between {MIN_ALNUM_RUN} and "
            f"{MAX_ALNUM_RUN_CEILING}; below that the tool's own synthetic tokens would "
            "fail the self-check, above it the rule stops catching signed blobs"
        )
    if args.date_shift_days == 0:
        raise RedactError("--date-shift-days 0: a zero shift leaves real dates in the output")
    for name in args.listing_dir or ():
        if (
            name in ("", ".", "..")
            or "/" in name
            or "\\" in name
            or os.sep in name
            or any(char in name for char in "*?[]")
        ):
            raise RedactError(
                f"--listing-dir {name!r}: must be a bare directory name, with no path "
                "separator and no glob character (it is used as an output filename)"
            )
    for name in args.keep_keys_in:
        if name not in DEFAULT_KEY_MASK_PATHS:
            raise RedactError(
                f"--keep-keys-in {name!r}: not a built-in entry, so nothing to turn off "
                f"(built in: {', '.join(DEFAULT_KEY_MASK_PATHS)})"
            )
    src = args.src.resolve()
    dst = args.dst.resolve()
    if src == dst:
        raise RedactError("src and dst are the same directory: pick a separate dst")
    if dst.is_relative_to(src) or src.is_relative_to(dst):
        raise RedactError(
            f"dst {dst} and src {src} are nested: pick a dst outside the export tree"
        )
    if args.dst.exists() and not args.dst.is_dir():
        raise RedactError(f"dst {args.dst} exists and is not a directory: pick another dst")
    if args.dst.is_dir() and any(args.dst.iterdir()) and not args.force:
        raise RedactError(f"dst {args.dst} is not empty: pick an empty dir or pass --force")


def resolve_shift(seed: int, date_shift_days: int | None) -> timedelta:
    if date_shift_days is not None:
        return timedelta(days=date_shift_days)
    rng = random.Random(seed_int(seed, "shift"))
    return timedelta(days=rng.randrange(-2200, -366), seconds=rng.randrange(0, 86400))


def scan_source(src: Path) -> tuple[list[Path], list[str]]:
    """One walk, no symlink following: json files plus the symlinked dirs skipped."""
    json_files: list[Path] = []
    skipped: list[str] = []
    for root, dirs, files in os.walk(src, followlinks=False):
        root_path = Path(root)
        for name in sorted(dirs):
            if (root_path / name).is_symlink():
                skipped.append((root_path / name).relative_to(src).as_posix())
        for name in sorted(files):
            if name.lower().endswith(".json"):
                json_files.append(root_path / name)
    return sorted(json_files), sorted(skipped)


def find_listing_dirs(src: Path, names: Sequence[str]) -> list[tuple[str, Path]]:
    found = []
    for name in names:
        for path in sorted(src.rglob(name)):
            if path.is_dir():
                found.append((name, path))
    return found


def unique_rel(rel: str, taken: set) -> str:
    """Two source names can mask to the same output name; never overwrite."""
    if rel not in taken:
        return rel
    stem, ext = os.path.splitext(rel)
    for index in range(2, 1000):
        candidate = f"{stem}-{index}{ext}"
        if candidate not in taken:
            return candidate
    raise RedactError(f"too many masked names collide on {rel}: rename the sources")


def write_json(path: Path, payload: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.unlink(missing_ok=True)  # never write through a pre-existing symlink
    path.write_text(
        json.dumps(payload, indent=2, ensure_ascii=False, sort_keys=False) + "\n",
        encoding="utf-8",
    )


HANDLE_SEGMENT_RE = re.compile(r"(user|conv|id|key)_\d+")


def segment_is_schema(segment: str) -> bool:
    """A pointer segment may be echoed only if it cannot be payload: an array
    index, a handle this tool minted, or a known schema label."""
    return (
        segment == ""
        or segment.isdigit()
        or HANDLE_SEGMENT_RE.fullmatch(segment) is not None
        or normalize_label(segment) in SCHEMA_KEYS
    )


def report_pointer(pointer: str) -> str:
    """Inverted like the value model: keep a segment only when it is provably not
    payload, so a heuristic miss over-masks the report instead of leaking."""
    return "/".join(
        segment if segment_is_schema(segment) else DYNAMIC_KEY_PLACEHOLDER
        for segment in pointer.split("/")
    )


def suggest_rule(container: dict) -> str:
    """A pasteable --mask-keys-under rule. Anything that is not a known schema
    label becomes '*', which matches any single segment, so the rule stays
    usable without naming a key. The file is named by its MIRRORED path, which
    is what --mask-keys-under matches against."""
    pointer = "/".join(
        segment if normalize_label(segment) in SCHEMA_KEYS or segment == "" else "*"
        for segment in container["pointer"].split("/")
    )
    return f"{os.path.basename(container['file'])}:{pointer or '/'}"


def looks_id_keyed(container: dict, key_containers: Counter) -> bool:
    """Keep only candidates whose keys are neither schema labels nor repeated
    across sibling containers: a schema label shows up in every record and in
    SCHEMA_KEYS, a username in exactly one container and in neither."""
    names = [
        key for key in container["key_names"] if normalize_label(key) not in SCHEMA_KEYS
    ]
    if not names:
        return False  # every key is a known schema label: an object, not a map
    unique = sum(1 for key in names if key_containers[key] == 1)
    return unique * 2 >= len(names)


def format_counter(counter: Counter) -> str:
    if not counter:
        return "none"
    return ", ".join(f"{name}={count}" for name, count in sorted(counter.items()))


def print_summary(stats: Stats, listings: Sequence[dict], rules: Sequence[MaskRule]) -> None:
    print(f"\n{TOOL}: summary (counts only)")
    print(f"  json files redacted : {stats.json_files}")
    print(f"  leaves by type      : {format_counter(stats.leaf_types)}")
    print(f"  values by class     : {format_counter(stats.value_classes)}")
    print(f"  arrays sampled      : {stats.truncated_arrays} (true lengths in {REPORT_NAME})")
    print(f"  keys kept verbatim  : {len(stats.keys)} distinct")
    print(f"  keys masked         : {stats.masked_keys} ({stats.kept_schema_keys} schema keys kept)")
    print(f"  paths masked        : {len(stats.renamed_paths)} name(s) not in the vocabulary")
    print(f"  vocabulary used     : {len(stats.vocabulary_used)} member(s), listed in {REPORT_NAME}")
    for rule in rules:
        origin = "built-in" if rule.builtin else "requested"
        print(
            f"  key mask ({origin}): {rule.glob} at "
            f"{mask_pointer('/'.join(rule.segments)) or '/'} -> {rule.hits} container(s)"
        )
    for listing in listings:
        print(
            f"  listing {listing['dir']}: {listing['file_count']} files, "
            f"{len(listing['patterns'])} filename patterns"
        )
        for entry in listing["patterns"]:
            print(f"      {entry['count']:>7}  {entry['pattern']}")
    suspects = [
        container
        for container in stats.dynamic_containers
        if looks_id_keyed(container, stats.key_containers)
    ]
    if suspects:
        print(
            f"  advisory: {len(suspects)} container(s) hold >= {DYNAMIC_KEY_HINT} keys with "
            "uniform values that do not repeat across siblings, which looks like a map keyed "
            "by id. Those keys are kept verbatim -- if they are usernames or ids, re-run with:"
        )
        for container in suspects:
            pointer = report_pointer(container["pointer"])
            print(
                f"      --mask-keys-under {suggest_rule(container)!r}"
                f"   ({container['keys']} keys of {container['values']} at "
                f"{container['file']}{json_pointer(pointer)})"
            )


def run(args: argparse.Namespace) -> int:
    validate(args)
    seed = args.seed if args.seed is not None else secrets.randbits(64)
    user_rules = [parse_mask_rule(raw) for raw in args.mask_keys_under]
    rules = builtin_mask_rules(args.keep_keys_in) + user_rules
    ctx = Ctx(
        seed=seed,
        shift=resolve_shift(seed, args.date_shift_days),
        array_sample=args.array_sample,
        max_alnum_run=args.max_alnum_run,
        mask_rules=rules,
        stats=Stats(),
        handles=Handles(seed),
        show_source_names=args.show_source_names,
    )
    src, dst = args.src, args.dst
    dst.mkdir(parents=True, exist_ok=True)

    json_files, skipped_links = scan_source(src)
    if not json_files:
        raise RedactError(f"no *.json found under {src}: is this the export root?")
    if skipped_links:
        print(f"  note: {len(skipped_links)} symlinked dir(s) under src were not walked")
    file_reports = []
    mirror_rels: list[str] = []
    taken: set = set()
    for src_file in json_files:
        source_rel = src_file.relative_to(src).as_posix()
        # Rule matching uses the pre-dedup name, so two sources that mask to one
        # name are covered by one rule instead of silently needing two.
        match_rel = mask_relative_path(source_rel)
        rel = unique_rel(match_rel, taken)
        taken.add(rel)
        if match_rel != source_rel:
            ctx.stats.renamed_paths.append(rel)
        ctx.rel_aliases = (match_rel, rel)
        redacted, truncations = redact_file(src_file, source_rel, ctx.rel_aliases, ctx)
        write_json(dst / rel, redacted)
        mirror_rels.append(rel)
        file_reports.append({"path": rel, "truncated_arrays": truncations})
        print(f"  {rel}: {len(truncations)} array(s) sampled")

    if ctx.stats.renamed_paths:
        print(
            f"  note: {len(ctx.stats.renamed_paths)} name(s) held a word outside "
            "NAME_VOCABULARY and were rewritten. --mask-keys-under matches these names:"
        )
        for name in ctx.stats.renamed_paths:
            print(f"      {name}")
    unused = [rule for rule in user_rules if rule.hits == 0]
    if unused:
        raise RedactError(
            "--mask-keys-under matched no container: "
            + ", ".join(repr(rule.raw) for rule in unused)
            + ". A rule matches the MIRRORED name, the one listed above, not the source "
            "name you typed -- a name holding a word outside NAME_VOCABULARY is rewritten. "
            f"Nothing was masked there, so the mirror in {dst} still holds those keys: "
            "delete it, re-check the name and the pointer against the lines above, then re-run"
        )
    for entry in file_reports:
        for truncation in entry["truncated_arrays"]:
            truncation["pointer"] = json_pointer(report_pointer(truncation["pointer"]))

    listing_names = args.listing_dir or list(DEFAULT_LISTING_DIRS)
    listings = []
    found = find_listing_dirs(src, listing_names)
    for name, dir_path in found:
        listing = build_listing(dir_path, name, ctx, args.examples)
        write_json(dst / LISTINGS_DIRNAME / f"{listing['dir']}.json", listing)
        listings.append(listing)
    missing = sorted(mask_name(name) for name in set(listing_names) - {n for n, _ in found})
    if missing:
        print(f"  note: no dir named {', '.join(missing)} under src, so no listing for it")

    report = {
        "tool": TOOL,
        "format": 3,
        "note": "the seed and the date shift are deliberately not recorded here",
        "settings": {
            "array_sample": args.array_sample,
            "examples": args.examples,
            "max_alnum_run": args.max_alnum_run,
            "listing_dirs": [mask_name(name) for name in listing_names],
            "key_mask_rules": [
                {
                    "origin": "built-in" if rule.builtin else "requested",
                    "file_glob": rule.glob,
                    "pointer": mask_pointer("/".join(rule.segments)) or "/",
                    "containers_masked": rule.hits,
                }
                for rule in rules
            ],
            "keep_keys_in": args.keep_keys_in,
        },
        "totals": {
            "json_files": ctx.stats.json_files,
            "leaf_types": dict(sorted(ctx.stats.leaf_types.items())),
            "value_classes": dict(sorted(ctx.stats.value_classes.items())),
            "truncated_arrays": ctx.stats.truncated_arrays,
            "distinct_keys_kept": len(ctx.stats.keys),
            "keys_masked": ctx.stats.masked_keys,
            "schema_keys_kept_inside_masked_containers": ctx.stats.kept_schema_keys,
            "paths_masked": len(ctx.stats.renamed_paths),
            "join_handles": len(ctx.handles.table),
        },
        # Closed-vocabulary members that were kept, so they can be audited.
        "vocabulary_used": sorted(ctx.stats.vocabulary_used),
        "files": file_reports,
        "listings": [f"{LISTINGS_DIRNAME}/{listing['dir']}.json" for listing in listings],
        "self_check": {"rules": list(SELF_CHECK_RULES)},
    }
    report_path = dst / REPORT_NAME
    write_json(report_path, report)

    print_summary(ctx.stats, listings, rules)
    failures = self_check(dst, ctx.stats, mirror_rels, args.max_alnum_run)
    report["self_check"] = {
        "rules": list(SELF_CHECK_RULES),
        "passed": not failures,
        "failures": len(failures),
    }
    write_json(report_path, report)
    if failures:
        print(f"\n{TOOL}: SELF-CHECK FAILED ({len(failures)} violation(s)):", file=sys.stderr)
        for line in failures[:20]:
            print(f"  {line}", file=sys.stderr)
        if len(failures) > 20:
            print(f"  ... and {len(failures) - 20} more", file=sys.stderr)
        print(
            f"{TOOL}: {dst} may still contain real data. Do not copy it into fixtures.",
            file=sys.stderr,
        )
        return EXIT_SELF_CHECK
    print(f"\n{TOOL}: self-check passed ({len(SELF_CHECK_RULES)} rules). Output: {dst}")
    return 0


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        return run(args)
    except RedactError as exc:
        print(f"{TOOL}: error: {exc}", file=sys.stderr)
        return EXIT_CONFIG


if __name__ == "__main__":
    sys.exit(main())
