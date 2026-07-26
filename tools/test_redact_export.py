#!/usr/bin/env python3
"""Tests for redact_export.py. Every input here is fabricated: no real export
is read, and the marker values are nonsense strings chosen to be greppable.

Run: python3 tools/test_redact_export.py
"""

from __future__ import annotations

import contextlib
import io
import json
import os
import tempfile
import unittest
from collections import Counter
from datetime import timedelta
from pathlib import Path
from unittest import mock

import redact_export as rx

SHIFT_DAYS = "-400"
SEED = "1234"
UUID_IN = "1f3d4a5b-2c6e-4f8a-9b0c-1d2e3f4a5b6c"
UUID_MASK = "********-****-****-****-************"

# Values that must never reach the output. Nonsense, >= 6 alnum chars, and
# nothing an allowlist could excuse.
MARKERS = (
    "zzqqusernamemarker",
    "ZZQQMESSAGETEXTMARKER",
    "zzqqdisplaynamemarker",
    "ZZQQSIGNEDBLOBMARKER1234567890",
)


def shape(node):
    """Keys, nesting, array length and leaf TYPE, with every value dropped."""
    if isinstance(node, dict):
        return {key: shape(value) for key, value in node.items()}
    if isinstance(node, list):
        return [shape(item) for item in node]
    if node is None:
        return "null"
    if isinstance(node, bool):
        return "bool"
    if isinstance(node, int):
        return "int"
    if isinstance(node, float):
        return "float"
    return "str"


def echo_chain(target):
    """A copy of the synth chain whose `target` entry passes its input through."""
    return tuple(
        (name, (lambda value, ctx, path: value)) if name == target else (name, synth)
        for name, synth in rx.SYNTH_CHAIN
    )


class RedactorCase(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        self.src = self.root / "export"
        self.dst = self.root / "redacted"
        (self.src / "json").mkdir(parents=True)
        self.stdout = ""
        self.stderr = ""

    def write_json(self, name, payload):
        (self.src / "json" / name).write_text(
            json.dumps(payload, indent=2, ensure_ascii=False), encoding="utf-8"
        )

    def write_raw(self, name, text):
        (self.src / "json" / name).write_text(text, encoding="utf-8")

    def write_media(self, dirname, names):
        target = self.src / dirname
        target.mkdir(parents=True, exist_ok=True)
        for name in names:
            (target / name).write_bytes(b"")

    def run_tool(self, *extra, expect=0, dst=None, seed=SEED, shift=SHIFT_DAYS):
        argv = [str(self.src), str(dst or self.dst)]
        if seed is not None:
            argv += ["--seed", seed]
        if shift is not None:
            argv += ["--date-shift-days", shift]
        argv += list(extra)
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = rx.main(argv)
        self.stdout, self.stderr = out.getvalue(), err.getvalue()
        self.assertEqual(code, expect, msg=f"stdout:\n{self.stdout}\nstderr:\n{self.stderr}")
        return code

    def out_json(self, rel="json/data.json"):
        return json.loads((self.dst / rel).read_text(encoding="utf-8"))

    def report(self):
        return json.loads((self.dst / rx.REPORT_NAME).read_text(encoding="utf-8"))

    def listing(self, name):
        return json.loads(
            (self.dst / rx.LISTINGS_DIRNAME / f"{name}.json").read_text(encoding="utf-8")
        )

    def all_output_text(self):
        return "\n".join(
            path.read_text(encoding="utf-8")
            for path in sorted(self.dst.rglob("*"))
            if path.is_file()
        )

    def redact_one(self, value, key="Field", *extra):
        self.write_json("data.json", {key: value})
        self.run_tool(*extra)
        return self.out_json()[key]


class TestStructure(RedactorCase):
    def test_keys_nesting_and_types_survive(self):
        payload = {
            "Basic Information": {
                "Username": "zzqqusernamemarker",
                "Creation Date": "2021-01-15 20:26:14 UTC",
                "Verified": True,
                "Score": 4211,
                "Missing": None,
                "Blank": "",
            },
            "Devices": [
                {"Make": "zzqqdisplaynamemarker", "Ratio": 1.75},
                {"Make": "zzqqdisplaynamemarker", "Ratio": 2.5},
            ],
            "Deep": {"a": {"b": {"c": ["x", 1, 1.5, None, False]}}},
        }
        self.write_json("data.json", payload)
        self.run_tool()
        self.assertEqual(shape(self.out_json()), shape(payload))

    def test_unicode_key_kept_verbatim(self):
        self.write_json("data.json", {"Ünïcode Kéy": "zzqqusernamemarker"})
        self.run_tool()
        self.assertEqual(list(self.out_json()), ["Ünïcode Kéy"])

    def test_long_array_sampled_and_true_length_recorded(self):
        rows = [{"i": index, "Time": "2021-01-15 20:26:14 UTC"} for index in range(40)]
        self.write_json("data.json", {"Frequent Locations": rows})
        self.run_tool("--array-sample", "5")
        self.assertEqual(len(self.out_json()["Frequent Locations"]), 5)
        self.assertEqual(
            self.report()["files"],
            [
                {
                    "path": "json/data.json",
                    "truncated_arrays": [
                        {"pointer": "/Frequent Locations", "true_length": 40, "kept": 5}
                    ],
                }
            ],
        )
        self.assertEqual(self.report()["totals"]["truncated_arrays"], 1)

    def test_short_array_kept_whole_and_not_reported(self):
        self.write_json("data.json", {"Rows": [1, 2, 3]})
        self.run_tool("--array-sample", "5")
        self.assertEqual(len(self.out_json()["Rows"]), 3)
        self.assertEqual(self.report()["files"][0]["truncated_arrays"], [])

    def test_array_sample_zero_keeps_every_element(self):
        self.write_json("data.json", {"Rows": list(range(40))})
        self.run_tool("--array-sample", "0")
        self.assertEqual(len(self.out_json()["Rows"]), 40)
        self.assertEqual(self.report()["totals"]["truncated_arrays"], 0)

    def test_nested_array_true_length_uses_full_pointer(self):
        self.write_json("data.json", {"Outer": {"Inner": list(range(9))}})
        self.run_tool("--array-sample", "2")
        self.assertEqual(
            self.report()["files"][0]["truncated_arrays"],
            [{"pointer": "/<key>/<key>", "true_length": 9, "kept": 2}],
        )

    def test_null_and_empty_string_pass_through(self):
        self.write_json("data.json", {"n": None, "e": ""})
        self.run_tool()
        self.assertEqual(self.out_json(), {"n": None, "e": ""})

    def test_bool_stays_bool_and_is_not_treated_as_int(self):
        self.write_json("data.json", {"flags": [True, False]})
        self.run_tool()
        self.assertEqual([type(v) for v in self.out_json()["flags"]], [bool, bool])
        self.assertEqual(self.report()["totals"]["leaf_types"], {"bool": 2})

    def test_bools_are_re_rolled_not_copied(self):
        self.write_json("data.json", {"flags": [True] * 8})
        self.run_tool()
        # Path-derived coin flips: an all-true input cannot stay all-true.
        self.assertIn(False, self.out_json()["flags"])

    def test_plain_int_keeps_digit_count_and_sign(self):
        self.write_json("data.json", {"a": 4211, "b": -17, "c": 0})
        self.run_tool()
        out = self.out_json()
        self.assertEqual(len(str(out["a"])), 4)
        self.assertNotEqual(out["a"], 4211)
        self.assertGreaterEqual(out["b"], -99)
        self.assertLessEqual(out["b"], -10)
        self.assertEqual(len(str(abs(out["c"]))), 1)


class TestFormatPreserving(RedactorCase):
    def test_datetime_utc_shifted_exactly(self):
        self.assertEqual(self.redact_one("2021-01-15 20:26:14 UTC"), "2019-12-12 20:26:14 UTC")

    def test_iso_t_and_z_separators_survive(self):
        self.assertEqual(self.redact_one("2021-01-15T20:26:14Z"), "2019-12-12T20:26:14Z")

    def test_offset_suffix_survives(self):
        self.assertEqual(
            self.redact_one("2021-01-15 20:26:14 +02:00"), "2019-12-12 20:26:14 +02:00"
        )

    def test_fractional_seconds_keep_their_width_but_not_their_digits(self):
        out = self.redact_one("2021-01-15 20:26:14.123 UTC")
        self.assertRegex(out, r"^2019-12-12 20:26:14\.\d{3} UTC$")
        self.assertNotEqual(out, "2019-12-12 20:26:14.123 UTC")

    def test_date_only_shifted(self):
        self.assertEqual(self.redact_one("2021-01-15"), "2019-12-12")

    def test_us_slash_date_keeps_field_order(self):
        self.assertEqual(self.redact_one("01/15/2021"), "12/12/2019")

    def test_ymd_slash_date(self):
        self.assertEqual(self.redact_one("2021/01/15"), "2019/12/12")

    def test_date_shaped_but_impossible_falls_back_to_generic(self):
        self.assertRegex(self.redact_one("2021-13-45"), r"^redacted-[a-z0-9]{6}$")

    def test_latlon_label_kept_decimals_kept_sign_dropped(self):
        out = self.redact_one("Latitude, Longitude: 45.123456, -13.4")
        self.assertRegex(out, r"^Latitude, Longitude: 1\.\d{6}, 1\.\d$")

    def test_bare_coord_pair_keeps_its_separator(self):
        self.assertRegex(self.redact_one("45.5,13.25"), r"^1\.\d,1\.\d{2}$")

    def test_free_text_before_a_coord_pair_is_destroyed_not_echoed(self):
        out = self.redact_one("Zzqqdisplayname Marker: 45.5, -13.25")
        self.assertRegex(out, r"^redacted-[a-z0-9]{6}$")

    def test_every_allowlisted_coord_label_is_kept(self):
        for index, label in enumerate(
            ("Latitude, Longitude:", "Lat, Lon:", "Location:", "Coordinates:")
        ):
            with self.subTest(label=label):
                self.dst = self.root / f"out{index}"
                out = self.redact_one(f"{label} 45.5, -13.25")
                self.assertTrue(out.startswith(label), msg=out)

    def test_coordinates_land_in_the_fake_band(self):
        out = self.redact_one("Latitude, Longitude: 45.123456, -13.499999")
        lat, lon = (float(part) for part in out.split(": ", 1)[1].split(", "))
        for value in (lat, lon):
            self.assertGreaterEqual(value, rx.FAKE_COORD_LO)
            self.assertLessEqual(value, rx.FAKE_COORD_HI)

    def test_url_keeps_scheme_host_and_param_names_only(self):
        url = (
            "https://app.snapchat.com/dmd/memories"
            "?uid=ZZQQSIGNEDBLOBMARKER1234567890&sid=zzqqusernamemarker&mid=7"
        )
        self.assertEqual(
            self.redact_one(url),
            "https://app.snapchat.com/REDACTED/REDACTED"
            "?uid=REDACTED&sid=REDACTED&mid=REDACTED",
        )

    def test_query_chunk_without_an_equals_is_all_payload(self):
        self.assertEqual(
            self.redact_one("https://cf-st.sc-cdn.net/media?zzqqusernamemarker"),
            "https://cf-st.sc-cdn.net/REDACTED?REDACTED",
        )

    def test_query_chunk_with_an_unnameable_key_is_all_payload(self):
        self.assertEqual(
            self.redact_one("https://cf-st.sc-cdn.net/a?zzqq%20user=1&uid=2"),
            "https://cf-st.sc-cdn.net/REDACTED?REDACTED&uid=REDACTED",
        )

    def test_url_userinfo_is_dropped(self):
        self.assertEqual(
            self.redact_one("https://zzqqusernamemarker:hunter2@app.snapchat.com/a"),
            "https://app.snapchat.com/REDACTED",
        )

    def test_url_fragment_is_redacted(self):
        self.assertEqual(
            self.redact_one("https://app.snapchat.com/a#zzqqusernamemarker"),
            "https://app.snapchat.com/REDACTED#REDACTED",
        )

    def test_url_vocabulary_kept_is_enumerated_in_the_report(self):
        self.redact_one("https://app.snapchat.com/a?uid=1")
        self.assertEqual(self.report()["vocabulary_used"], ["app.snapchat.com", "uid"])

    def test_an_unknown_url_host_is_synthesized_not_kept(self):
        out = self.redact_one("https://zzqqusernamemarker.example.test/a?uid=1")
        self.assertRegex(out, r"^https://redacted-[a-z0-9]{6}\.invalid/REDACTED\?uid=REDACTED$")

    def test_media_filename_keeps_pattern_zeroes_uuid_shifts_date(self):
        self.assertEqual(
            self.redact_one(f"2021-01-15_media~zip-{UUID_IN}.jpg"),
            f"2019-12-12_media~zip-{rx.ZERO_UUID}.jpg",
        )

    def test_overlay_filename_keeps_its_role_word(self):
        self.assertEqual(
            self.redact_one(f"2021-01-15_overlay~zip-{UUID_IN}.png"),
            f"2019-12-12_overlay~zip-{rx.ZERO_UUID}.png",
        )

    def test_filename_word_payload_is_masked_not_kept(self):
        self.assertEqual(
            self.redact_one("zzqqusernamemarker-media.mp4"), "xxxxxxxxxxxx-media.mp4"
        )

    def test_adjacent_filename_payload_pieces_stay_separate_tokens(self):
        self.assertEqual(self.redact_one("123zzqqusernamemarker.jpg"), "xxx-xxxxxxxxxxxx.jpg")

    def test_enum_values_pass_through_verbatim(self):
        self.write_json("data.json", {"a": "PHOTO", "b": "VIDEO", "c": "TEXT"})
        self.run_tool()
        self.assertEqual(self.out_json(), {"a": "PHOTO", "b": "VIDEO", "c": "TEXT"})

    def test_an_enum_word_under_a_name_key_is_not_an_enum(self):
        self.write_json("data.json", {"Display Name": "SNAP", "Nick": "NONE", "Type": "NONE"})
        self.run_tool()
        out = self.out_json()
        self.assertEqual(out["Display Name"], "user_1")  # a join key, so a handle
        self.assertRegex(out["Nick"], r"^redacted-[a-z0-9]{6}$")
        self.assertEqual(out["Type"], "NONE")

    def test_lowercase_lookalike_of_an_enum_is_not_an_enum(self):
        self.assertRegex(self.redact_one("photo"), r"^redacted-[a-z0-9]{6}$")

    def test_bare_extension_words_pass_through(self):
        self.write_json("data.json", {"a": ".jpg", "b": "jpeg"})
        self.run_tool()
        self.assertEqual(self.out_json(), {"a": ".jpg", "b": "jpeg"})

    def test_standalone_uuid_is_zeroed(self):
        self.assertEqual(self.redact_one(UUID_IN), rx.ZERO_UUID)

    def test_epoch_micros_shift_by_exactly_the_offset(self):
        self.assertEqual(
            self.redact_one(1610742374000000), 1610742374000000 - 400 * 86400 * 10**6
        )

    def test_epoch_seconds_shift_by_exactly_the_offset(self):
        self.assertEqual(self.redact_one(1610742374), 1610742374 - 400 * 86400)

    def test_epoch_millis_shift_by_exactly_the_offset(self):
        self.assertEqual(self.redact_one(1610742374000), 1610742374000 - 400 * 86400 * 10**3)

    def test_digit_string_epoch_shifts_and_stays_a_string(self):
        self.assertEqual(self.redact_one("1610742374"), str(1610742374 - 400 * 86400))

    def test_non_epoch_digit_string_keeps_its_width_but_not_its_value(self):
        out = self.redact_one("5551234")
        self.assertRegex(out, r"^\d{7}$")
        self.assertNotEqual(out, "5551234")

    def test_float_keeps_magnitude_and_decimal_count_but_not_its_value(self):
        out = self.redact_one(12.75)
        self.assertRegex(repr(out), r"^\d{2}\.\d{1,2}$")
        self.assertNotEqual(out, 12.75)

    def test_negative_float_keeps_its_sign(self):
        self.assertLess(self.redact_one(-12.75), 0)

    def test_latitude_keyed_float_lands_in_the_fake_band(self):
        out = self.redact_one(45.987654, "Latitude")
        self.assertGreaterEqual(out, rx.FAKE_COORD_LO)
        self.assertLessEqual(out, rx.FAKE_COORD_HI)
        self.assertEqual(self.report()["totals"]["value_classes"]["coord float"], 1)

    def test_an_enum_under_a_type_key_survives_the_free_text_guard(self):
        self.write_json(
            "data.json", {"Message Type": "PHOTO", "Content Type": "VIDEO", "Note Type": "TEXT"}
        )
        self.run_tool()
        self.assertEqual(
            self.out_json(), {"Message Type": "PHOTO", "Content Type": "VIDEO", "Note Type": "TEXT"}
        )

    def test_a_date_under_a_join_key_keeps_its_format(self):
        self.write_json("data.json", {"From": "2021-01-15", "To": "2021-02-20 10:00:00 UTC"})
        self.run_tool()
        self.assertEqual(
            self.out_json(), {"From": "2019-12-12", "To": "2020-01-17 10:00:00 UTC"}
        )

    def test_a_url_under_a_join_key_keeps_its_shape(self):
        self.assertEqual(
            self.redact_one("https://app.snapchat.com/a?uid=1", "From"),
            "https://app.snapchat.com/REDACTED?uid=REDACTED",
        )

    def test_a_coordinate_under_a_coord_parent_key_lands_in_the_band(self):
        self.write_json("data.json", {"geo": {"x": 45.5123, "y": -13.2567}})
        self.run_tool()
        for value in self.out_json()["geo"].values():
            self.assertGreaterEqual(value, rx.FAKE_COORD_LO)
            self.assertLessEqual(value, rx.FAKE_COORD_HI)

    def test_numeric_coordinate_array_lands_in_the_fake_band(self):
        self.write_json("data.json", {"Coordinates": [45.5123, -13.2567]})
        self.run_tool()
        for value in self.out_json()["Coordinates"]:
            self.assertGreaterEqual(value, rx.FAKE_COORD_LO)
            self.assertLessEqual(value, rx.FAKE_COORD_HI)


class TestListings(RedactorCase):
    def test_patterns_counts_and_masked_examples(self):
        self.write_json("data.json", {"a": 1})
        self.write_media(
            "chat_media",
            [
                f"2021-01-15_media~zip-{UUID_IN}.jpg",
                "2021-02-20_media~zip-aaaabbbb-cccc-dddd-eeee-ffff00001111.jpg",
                f"2021-01-15_overlay~zip-{UUID_IN}.png",
            ],
        )
        self.run_tool()
        self.assertEqual(
            self.listing("chat_media"),
            {
                "dir": "chat_media",
                "file_count": 3,
                "subdir_count": 0,
                "patterns": [
                    {
                        "pattern": "YYYY-MM-DD_media~zip-<uuid>.jpg",
                        "count": 2,
                        "examples": [
                            f"****-**-**_media~zip-{UUID_MASK}.jpg",
                            f"****-**-**_media~zip-{UUID_MASK}.jpg",
                        ],
                    },
                    {
                        "pattern": "YYYY-MM-DD_overlay~zip-<uuid>.png",
                        "count": 1,
                        "examples": [f"****-**-**_overlay~zip-{UUID_MASK}.png"],
                    },
                ],
            },
        )

    def test_examples_flag_caps_the_sample(self):
        self.write_json("data.json", {"a": 1})
        self.write_media(
            "memories",
            [
                f"2021-01-1{index}_media~zip-aaaabbbb-cccc-dddd-eeee-ffff0000111{index}.jpg"
                for index in range(4)
            ],
        )
        self.run_tool("--examples", "1")
        patterns = self.listing("memories")["patterns"]
        self.assertEqual(patterns[0]["count"], 4)
        self.assertEqual(len(patterns[0]["examples"]), 1)

    def test_unknown_filename_words_become_length_only_tokens(self):
        self.write_json("data.json", {"a": 1})
        self.write_media("chat_media", ["zzqqusernamemarker-media.jpg"])
        self.run_tool()
        listing = self.listing("chat_media")
        self.assertEqual(listing["patterns"][0]["pattern"], "<w:18>-media.jpg")
        self.assertEqual(listing["patterns"][0]["examples"], ["******************-media.jpg"])
        self.assertNotIn("zzqqusernamemarker", self.all_output_text())

    def test_every_real_export_schema_filename_survives_verbatim(self):
        # A word missing from NAME_VOCABULARY renames the file a parser looks for,
        # and two mangled names can collide onto one mirror (snap_ads / snap_pro),
        # which also makes any --mask-keys-under rule against them ambiguous.
        names = [
            "account.json",
            "account_history.json",
            "bitmoji.json",
            "chat_history.json",
            "custom_sticker.json",
            "email_campaign_history.json",
            "feature_emails.json",
            "friends.json",
            "location_history.json",
            "memories_history.json",
            "ranking.json",
            "snap_ads.json",
            "snap_history.json",
            "snap_pro.json",
            "snapchat_ai.json",
            "snapchat_plus.json",
            "story_history.json",
            "terms_history.json",
            "user_profile.json",
        ]
        for name in names:
            self.write_json(name, {"Field": "value"})
        self.run_tool()
        mirrored = sorted(path.name for path in (self.dst / "json").glob("*.json"))
        self.assertEqual(mirrored, sorted(names))

    def test_non_ascii_filename_chars_are_payload_not_separators(self):
        self.write_json("data.json", {"a": 1})
        self.write_media("chat_media", ["привет-media.jpg"])
        self.run_tool()
        listing = self.listing("chat_media")
        self.assertEqual(listing["patterns"][0]["pattern"], "<u:7>media.jpg")
        self.assertEqual(listing["patterns"][0]["examples"], ["*******media.jpg"])
        self.assertNotIn("привет", self.all_output_text())

    def test_a_missing_listing_dir_is_called_out_not_silently_skipped(self):
        self.write_json("data.json", {"a": 1})
        self.run_tool()
        self.assertIn("no dir named chat_media, memories under src", self.stdout)
        self.assertFalse((self.dst / rx.LISTINGS_DIRNAME).exists())

    def test_subdirs_are_counted_and_walked(self):
        self.write_json("data.json", {"a": 1})
        self.write_media(
            "chat_media/2021", ["2021-01-15_media~zip-aaaabbbb-cccc-dddd-eeee-ffff00001111.jpg"]
        )
        self.run_tool()
        listing = self.listing("chat_media")
        self.assertEqual((listing["file_count"], listing["subdir_count"]), (1, 1))

    def test_a_listing_dir_name_may_not_escape_the_output_tree(self):
        self.write_json("data.json", {"a": 1})
        self.run_tool("--listing-dir", "../../..", expect=rx.EXIT_CONFIG)
        self.assertIn("bare directory name", self.stderr)

    def test_a_glob_listing_dir_name_is_refused(self):
        self.write_json("data.json", {"a": 1})
        self.run_tool("--listing-dir", "*", expect=rx.EXIT_CONFIG)
        self.assertIn("glob character", self.stderr)


class TestNothingRealSurvives(RedactorCase):
    def test_no_marker_value_reaches_the_output(self):
        self.write_json(
            "data.json",
            {
                "Username": "zzqqusernamemarker",
                "Content": "ZZQQMESSAGETEXTMARKER and more prose",
                "Nested": [{"Display Name": "zzqqdisplaynamemarker"}],
                "Link": "https://app.snapchat.com/a?sig=ZZQQSIGNEDBLOBMARKER1234567890",
                "Mixed": "prefix ZZQQMESSAGETEXTMARKER suffix",
            },
        )
        self.write_media("memories", ["zzqqdisplaynamemarker-media.jpg"])
        self.run_tool()
        text = self.all_output_text().lower()
        for marker in MARKERS:
            self.assertNotIn(marker.lower(), text)

    def test_no_marker_value_reaches_stdout_or_stderr(self):
        self.write_json(
            "data.json",
            {"Username": "zzqqusernamemarker", "Loc": "Latitude, Longitude: 45.5123, -13.2567"},
        )
        self.write_media("memories", ["zzqqdisplaynamemarker-media.jpg"])
        self.run_tool()
        printed = (self.stdout + self.stderr).lower()
        for marker in MARKERS:
            self.assertNotIn(marker.lower(), printed)
        self.assertNotIn("45.5123", printed)

    def test_generic_strings_do_not_echo_their_length_or_content(self):
        self.write_json("data.json", {"a": "zzqqusernamemarker", "b": "zzqqusernamemarker"})
        self.run_tool()
        out = self.out_json()
        for value in out.values():
            self.assertRegex(value, r"^redacted-[a-z0-9]{6}$")
        # Path-derived, so identical inputs do not collapse into one token.
        self.assertNotEqual(out["a"], out["b"])


class TestSelfCheck(RedactorCase):
    """Positive controls: the check must go red on planted leaks."""

    def check(self, files, generated=(), numbers=(), max_run=20):
        target = self.root / "planted"
        target.mkdir(exist_ok=True)
        for name, text in files.items():
            (target / name).write_text(text, encoding="utf-8")
        stats = rx.Stats()
        stats.generated.update(generated)
        stats.generated_numbers.update(numbers)
        return rx.self_check(target, stats, list(files), max_run)

    def test_clean_output_passes(self):
        clean = {
            "Date": "2019-12-12 20:26:14 UTC",
            "Link": "https://app.snapchat.com/REDACTED?uid=REDACTED",
            "Location": "Latitude, Longitude: 1.234567, 1.5",
            "Name": "redacted-ab12cd",
            "File": f"2019-12-12_media~zip-{rx.ZERO_UUID}.jpg",
            "Count": 4211,
        }
        self.assertEqual(
            self.check(
                {"a.json": json.dumps(clean)},
                generated={"234567", "ab12cd"},
                numbers={4211},
            ),
            [],
        )

    def test_url_with_a_live_path_segment_fails(self):
        leaky = json.dumps({"Link": "https://app.snapchat.com/dmd/stillhere?uid=REDACTED"})
        failures = self.check({"a.json": leaky})
        self.assertIn(rx.RULE_URL, failures[0])
        self.assertIn("a path segment", failures[0])

    def test_url_with_a_live_param_value_fails(self):
        leaky = json.dumps({"Link": "https://app.snapchat.com/REDACTED?uid=stillhere"})
        failures = self.check({"a.json": leaky})
        self.assertIn("a parameter payload", failures[0])

    def test_url_query_chunk_without_an_equals_fails(self):
        leaky = json.dumps({"Link": "https://app.snapchat.com/REDACTED?stillhere"})
        failures = self.check({"a.json": leaky})
        self.assertIn("a parameter payload", failures[0])

    def test_url_with_userinfo_fails(self):
        leaky = json.dumps({"Link": "https://user:pw@app.snapchat.com/REDACTED"})
        failures = self.check({"a.json": leaky})
        self.assertIn("userinfo", failures[0])

    def test_long_alnum_run_fails_with_its_length(self):
        failures = self.check({"a.json": json.dumps({"Blob": "a1b2c3d4e5f6g7h8i9j0k1"})})
        self.assertIn(rx.RULE_ALNUM, failures[0])
        self.assertIn("22 chars", failures[0])

    def test_alnum_run_in_a_key_also_fails(self):
        failures = self.check({"a.json": json.dumps({"a1b2c3d4e5f6g7h8i9j0k1": 1})})
        self.assertIn(rx.RULE_ALNUM, failures[0])

    def test_coord_pair_outside_the_fake_band_fails(self):
        failures = self.check({"a.json": json.dumps({"Loc": "Latitude, Longitude: 45.5, 13.25"})})
        self.assertEqual(len(failures), 1)
        self.assertIn(rx.RULE_COORD, failures[0])

    def test_in_band_coord_pair_passes(self):
        clean = json.dumps({"Loc": "Latitude, Longitude: 1.5, 1.25"})
        self.assertEqual(self.check({"a.json": clean}), [])

    def test_an_unaccounted_value_token_fails_and_is_reported_masked(self):
        failures = self.check({"a.json": json.dumps({"x": "ZZQQMARKER01"})})
        self.assertEqual(
            failures, [f"a.json at /*: {rx.RULE_VALUES} -- a 12-char token (************)"]
        )

    def test_a_generated_token_is_not_a_leak(self):
        failures = self.check({"a.json": json.dumps({"x": "abc123"})}, generated={"abc123"})
        self.assertEqual(failures, [])

    def test_allowlisted_words_are_never_counted_as_leaks(self):
        failures = self.check({"a.json": json.dumps({"x": "overlay", "y": "SCREENSHOT"})})
        self.assertEqual(failures, [])

    def test_an_unaccounted_number_fails(self):
        failures = self.check({"a.json": json.dumps({"n": 5551234})})
        self.assertEqual(len(failures), 1)
        self.assertIn(rx.RULE_NUMBERS, failures[0])

    def test_a_generated_number_passes(self):
        self.assertEqual(self.check({"a.json": json.dumps({"n": 5551234})}, numbers={5551234}), [])

    def test_bools_and_nulls_are_not_numbers(self):
        payload = json.dumps({"a": True, "b": False, "c": None})
        self.assertEqual(self.check({"a.json": payload}), [])

    def test_failure_lines_never_print_the_value_or_the_key(self):
        leaky = json.dumps({"zzqqusernamemarker": "Latitude, Longitude: 45.5123, -13.2567"})
        joined = " ".join(self.check({"a.json": leaky}))
        self.assertNotIn("zzqqusernamemarker", joined)
        self.assertNotIn("45.5123", joined)
        self.assertNotIn("13.2567", joined)
        self.assertIn(rx.RULE_COORD, joined)

    def test_cli_exits_nonzero_when_the_url_branch_leaks(self):
        """End-to-end control: break one branch, the run must refuse."""
        self.write_json(
            "data.json",
            {"Link": "https://app.snapchat.com/dmd/ZZQQSIGNEDBLOBMARKER1234567890"},
        )
        with mock.patch.object(rx, "SYNTH_CHAIN", echo_chain("url")):
            self.run_tool(expect=rx.EXIT_SELF_CHECK)
        self.assertIn("SELF-CHECK FAILED", self.stderr)
        self.assertIn(rx.RULE_URL, self.stderr)
        self.assertIn("may still contain real data", self.stderr)

    def test_cli_exits_nonzero_when_a_short_string_branch_leaks(self):
        """Control for the value-accounting rule: no url, no long alnum run."""
        self.write_json("data.json", {"Serial": "5551234"})
        with mock.patch.object(rx, "SYNTH_CHAIN", echo_chain("digit string")):
            self.run_tool(expect=rx.EXIT_SELF_CHECK)
        self.assertIn(rx.RULE_VALUES, self.stderr)

    def test_cli_exits_nonzero_when_the_float_branch_leaks(self):
        """Control for the number-accounting rule."""
        self.write_json("data.json", {"Ratio": 12.75})
        with mock.patch.object(rx, "synth_float", lambda value, ctx, path, key: value):
            self.run_tool(expect=rx.EXIT_SELF_CHECK)
        self.assertIn(rx.RULE_NUMBERS, self.stderr)

    def test_report_records_the_verdict(self):
        self.write_json("data.json", {"a": "2021-01-15 20:26:14 UTC"})
        self.run_tool()
        self.assertEqual(
            self.report()["self_check"],
            {"rules": list(rx.SELF_CHECK_RULES), "passed": True, "failures": 0},
        )

    def test_report_records_a_failure_instead_of_omitting_it(self):
        self.write_json("data.json", {"Serial": "5551234"})
        with mock.patch.object(rx, "SYNTH_CHAIN", echo_chain("digit string")):
            self.run_tool(expect=rx.EXIT_SELF_CHECK)
        verdict = self.report()["self_check"]
        self.assertFalse(verdict["passed"])
        self.assertGreater(verdict["failures"], 0)


class TestKeyMasking(RedactorCase):
    def test_mask_keys_under_replaces_child_keys_in_order(self):
        self.write_json(
            "chat_history.json",
            {
                "zzqqusernamemarker": [{"Content": "hi"}],
                "zzqqdisplaynamemarker": [{"Content": "yo"}],
            },
        )
        self.run_tool("--mask-keys-under", "chat_history.json:/")
        out = self.out_json("json/chat_history.json")
        self.assertEqual(list(out), ["key_1", "key_2"])
        self.assertEqual(self.report()["totals"]["keys_masked"], 2)
        self.assertNotIn("zzqqusernamemarker", self.all_output_text())

    def test_keys_of_a_file_outside_the_builtin_list_are_kept_verbatim(self):
        self.write_json("data.json", {"zzqqusernamemarker": [1]})
        self.run_tool()
        self.assertEqual(list(self.out_json()), ["zzqqusernamemarker"])

    def test_builtin_list_masks_username_keys_with_no_flag(self):
        self.write_json(
            "chat_history.json",
            {"zzqqusernamemarker": [{"Content": "hi"}], "Sent Saved Chat History": [{"a": 1}]},
        )
        self.run_tool()
        out = self.out_json("json/chat_history.json")
        # A known schema label survives the built-in heuristic; a username does not.
        self.assertEqual(list(out), ["user_1", "Sent Saved Chat History"])
        self.assertNotIn("zzqqusernamemarker", self.all_output_text())
        self.assertIn("key mask (built-in): chat_history.json", self.stdout)

    def test_keep_keys_in_turns_off_one_builtin_entry(self):
        self.write_json("chat_history.json", {"zzqqusernamemarker": [1]})
        self.run_tool("--keep-keys-in", "chat_history.json")
        self.assertEqual(list(self.out_json("json/chat_history.json")), ["zzqqusernamemarker"])

    def test_keep_keys_in_rejects_a_file_that_is_not_built_in(self):
        self.write_json("data.json", {"a": 1})
        self.run_tool("--keep-keys-in", "data.json", expect=rx.EXIT_CONFIG)
        self.assertIn("not a built-in entry", self.stderr)

    def test_the_same_username_gets_the_same_handle_across_files_and_positions(self):
        self.write_json("chat_history.json", {"zzqqusernamemarker": [{"Content": "hi"}]})
        self.write_json("snap_history.json", {"zzqqusernamemarker": [{"From": "zzqqothername"}]})
        self.write_json("friends.json", {"Friends": [{"Username": "zzqqusernamemarker"}]})
        self.run_tool()
        key_handle = list(self.out_json("json/chat_history.json"))[0]
        value_handle = self.out_json("json/friends.json")["Friends"][0]["Username"]
        self.assertEqual(key_handle, value_handle)
        self.assertEqual(list(self.out_json("json/snap_history.json")), [key_handle])

    def test_a_join_uuid_is_stable_and_still_uuid_shaped(self):
        self.write_json("data.json", {"Media ID": UUID_IN, "Other": {"id": UUID_IN}})
        self.run_tool()
        out = self.out_json()
        self.assertRegex(out["Media ID"], rx.UUID_RE)
        self.assertEqual(out["Media ID"], out["Other"]["id"])
        self.assertNotEqual(out["Media ID"], rx.ZERO_UUID)

    def test_a_join_int_is_stable_and_keeps_its_width(self):
        self.write_json("data.json", {"Message ID": 5551234, "Other": {"messageid": 5551234}})
        self.run_tool()
        out = self.out_json()
        self.assertEqual(len(str(out["Message ID"])), 7)
        self.assertNotEqual(out["Message ID"], 5551234)
        self.assertEqual(out["Message ID"], out["Other"]["messageid"])

    def test_nested_pointer_masks_only_that_container(self):
        self.write_json("data.json", {"Outer": {"aaa": 1, "bbb": 2}, "Keep": {"ccc": 3}})
        self.run_tool("--mask-keys-under", "/Outer")
        out = self.out_json()
        self.assertEqual(list(out["Outer"]), ["key_1", "key_2"])
        self.assertEqual(list(out["Keep"]), ["ccc"])

    def test_a_wildcard_segment_masks_every_array_element(self):
        self.write_json("chat_history.json", [{"z0user": [1], "z1user": [2]}, {"z9user": [3]}])
        self.run_tool("--mask-keys-under", "chat_history.json:/*")
        out = self.out_json("json/chat_history.json")
        self.assertEqual([list(item) for item in out], [["key_1", "key_2"], ["key_3"]])

    def test_a_rule_that_matches_nothing_is_an_error_not_a_silent_no_op(self):
        self.write_json("chat_history.json", {"zzqqusernamemarker": [1]})
        self.run_tool("--mask-keys-under", "chat_history.json:/nope", expect=rx.EXIT_CONFIG)
        self.assertIn("matched no container", self.stderr)
        self.assertIn("'chat_history.json:/nope'", self.stderr)
        self.assertIn("delete it", self.stderr)

    def test_a_wrong_file_glob_is_an_error(self):
        self.write_json("data.json", {"a": {"b": 1, "c": 2}})
        self.run_tool("--mask-keys-under", "other.json:/a", expect=rx.EXIT_CONFIG)
        self.assertIn("matched no container", self.stderr)

    def test_the_report_counts_hits_per_rule_without_echoing_the_pointer(self):
        self.write_json("data.json", {"Outer": {"zzqqusernamemarker": 1, "bbb": 2}})
        self.run_tool("--mask-keys-under", "data.json:/Outer")
        requested = [
            rule
            for rule in self.report()["settings"]["key_mask_rules"]
            if rule["origin"] == "requested"
        ]
        self.assertEqual(
            requested,
            [
                {
                    "origin": "requested",
                    "file_glob": "data.json",
                    "pointer": "/*****",
                    "containers_masked": 1,
                }
            ],
        )

    def test_a_pointer_typed_into_the_flag_is_not_echoed_into_the_report(self):
        self.write_json("data.json", {"zzqqusernamemarker": {"a": 1, "b": 2}})
        self.run_tool("--mask-keys-under", "data.json:/zzqqusernamemarker")
        settings = json.dumps(self.report()["settings"])
        self.assertNotIn("zzqqusernamemarker", settings)
        self.assertIn("/******************", settings)

    def test_dynamic_key_map_advisory_prints_a_pasteable_rule(self):
        self.write_json("data.json", {f"user{index}": [index] for index in range(4)})
        self.run_tool()
        self.assertIn("advisory:", self.stdout)
        self.assertIn("--mask-keys-under 'data.json:/'", self.stdout)

    def test_advisory_for_a_map_inside_an_array_suggests_a_wildcard(self):
        self.write_json("data.json", [{f"user{index}": [index] for index in range(4)}])
        self.run_tool()
        self.assertIn("--mask-keys-under 'data.json:/*'", self.stdout)

    def test_repeated_schema_keys_raise_no_advisory(self):
        record = {"From": "a", "Media Type": "TEXT", "Created": "2021-01-15", "Content": "hi"}
        self.write_json("data.json", {"Rows": [dict(record), dict(record), dict(record)]})
        self.run_tool()
        # Four uniform string fields, but the key set repeats: a schema object.
        self.assertNotIn("advisory:", self.stdout)

    def test_mixed_value_types_raise_no_advisory(self):
        self.write_json("data.json", {"a": 1, "b": "x", "c": [1], "d": {"e": 1}})
        self.run_tool()
        self.assertNotIn("advisory:", self.stdout)

    def test_a_dynamic_key_is_replaced_in_the_reports_pointers(self):
        rows = list(range(30))
        self.write_json("data.json", {f"user{index}": rows for index in range(4)})
        self.run_tool("--array-sample", "5")
        pointers = [
            truncation["pointer"]
            for entry in self.report()["files"]
            for truncation in entry["truncated_arrays"]
        ]
        self.assertEqual(pointers, ["/<key>"] * 4)

    def test_a_schema_pointer_stays_readable_in_the_report(self):
        self.write_json("data.json", {"Saved Media": list(range(30))})
        self.run_tool("--array-sample", "5")
        self.assertEqual(
            self.report()["files"][0]["truncated_arrays"][0]["pointer"], "/Saved Media"
        )

    def test_bad_mask_rule_is_rejected_with_the_fix(self):
        self.write_json("data.json", {"a": 1})
        self.run_tool("--mask-keys-under", "nonsense", expect=rx.EXIT_CONFIG)
        self.assertIn("--mask-keys-under", self.stderr)
        self.assertIn("json/pointer", self.stderr)


class TestOutputChannels(RedactorCase):
    """Nothing but vocabulary reaches a pointer, a printed line, or a filename."""

    def test_a_small_id_keyed_map_does_not_leak_its_keys_into_the_report(self):
        rows = list(range(30))
        self.write_json("data.json", {f"zzqquser{index}": rows for index in range(3)})
        self.run_tool("--array-sample", "5")
        # The mirror keeps keys by default (documented); the REPORT must not.
        self.assertNotIn("zzqquser", json.dumps(self.report()))
        self.assertNotIn("zzqquser", self.stdout + self.stderr)
        pointers = [
            truncation["pointer"]
            for entry in self.report()["files"]
            for truncation in entry["truncated_arrays"]
        ]
        self.assertEqual(pointers, ["/<key>"] * 3)

    def test_mixed_value_kinds_do_not_leak_keys_into_the_report(self):
        self.write_json(
            "data.json",
            {"zzqqusernamemarker": list(range(30)), "zzqqotheruser": {"a": 1}},
        )
        self.run_tool("--array-sample", "5")
        self.assertNotIn("zzqq", json.dumps(self.report()))
        self.assertNotIn("zzqq", self.stdout + self.stderr)
        self.assertEqual(
            self.report()["files"][0]["truncated_arrays"][0]["pointer"], "/<key>"
        )

    def test_the_advisory_names_neither_an_ancestor_key_nor_a_source_filename(self):
        self.write_json(
            "chat_with_zzqqfilemarker.json",
            {"zzqqancestormarker": {f"zzqqu{index}": [index] for index in range(5)}},
        )
        self.run_tool()
        printed = self.stdout + self.stderr
        for marker in ("zzqqfilemarker", "zzqqancestormarker", "zzqqu0"):
            self.assertNotIn(marker, printed)
        self.assertIn("--mask-keys-under 'chat_with_xxxxxxxxxxxx.json:/*'", self.stdout)

    def test_the_pasteable_rule_actually_matches_on_the_next_run(self):
        self.write_json(
            "chat_with_zzqqfilemarker.json",
            {"zzqqancestormarker": {f"zzqqu{index}": [index] for index in range(5)}},
        )
        self.run_tool()
        self.dst = self.root / "again"
        self.run_tool("--mask-keys-under", "chat_with_xxxxxxxxxxxx.json:/*")
        out = self.out_json("json/chat_with_xxxxxxxxxxxx.json")
        self.assertEqual(sorted(list(out.values())[0]), ["key_1", "key_2", "key_3", "key_4", "key_5"])

    def test_a_listing_dir_name_is_export_content_and_gets_masked(self):
        self.write_json("data.json", {"a": 1})
        self.write_media("zzqqusernamemarker", ["2021-01-15_media.jpg"])
        self.run_tool("--listing-dir", "zzqqusernamemarker")
        self.assertNotIn("zzqqusernamemarker", self.all_output_text())
        self.assertNotIn("zzqqusernamemarker", self.stdout)
        self.assertEqual(self.listing("xxxxxxxxxxxx")["dir"], "xxxxxxxxxxxx")
        self.assertEqual(self.report()["settings"]["listing_dirs"], ["xxxxxxxxxxxx"])

    def test_one_rule_covers_two_sources_that_mask_to_the_same_name(self):
        self.write_json("chat_with_alice.json", {f"zzqqa{i}": [i] for i in range(3)})
        self.write_json("chat_with_becky.json", {f"zzqqb{i}": [i] for i in range(3)})
        self.run_tool("--mask-keys-under", "chat_with_xxxxx.json:/")
        first = self.out_json("json/chat_with_xxxxx.json")
        second = self.out_json("json/chat_with_xxxxx-2.json")
        # Rules match the pre-dedup name, so the whole colliding family is masked.
        self.assertEqual(list(first), ["key_1", "key_2", "key_3"])
        self.assertEqual(list(second), ["key_4", "key_5", "key_6"])
        requested = [
            rule
            for rule in self.report()["settings"]["key_mask_rules"]
            if rule["origin"] == "requested"
        ]
        self.assertEqual(requested[0]["containers_masked"], 2)

    def test_colliding_mirrors_keep_distinct_report_paths(self):
        self.write_json("chat_with_alice.json", {"a": 1})
        self.write_json("chat_with_becky.json", {"a": 1})
        self.run_tool()
        self.assertEqual(
            sorted(entry["path"] for entry in self.report()["files"]),
            ["json/chat_with_xxxxx-2.json", "json/chat_with_xxxxx.json"],
        )

    def test_the_zero_hit_error_names_the_mirrored_name_contract(self):
        self.write_json("chat_with_alice.json", {"a": {"b": 1}})
        self.run_tool("--mask-keys-under", "chat_with_alice.json:/a", expect=rx.EXIT_CONFIG)
        self.assertIn("MIRRORED name", self.stderr)
        self.assertIn("not the source name you typed", self.stderr)
        # The rename note is printed BEFORE the abort, on the run that needs it.
        self.assertIn("--mask-keys-under matches these names:", self.stdout)
        self.assertIn("json/chat_with_xxxxx.json", self.stdout)

    def test_a_deduped_name_resolves_to_exactly_one_mirror(self):
        self.write_json("chat_with_alice.json", {f"zzqqa{i}": [i] for i in range(3)})
        self.write_json("chat_with_becky.json", {f"zzqqb{i}": [i] for i in range(3)})
        self.run_tool("--mask-keys-under", "chat_with_xxxxx-2.json:/")
        self.assertEqual(
            list(self.out_json("json/chat_with_xxxxx.json")), ["zzqqa0", "zzqqa1", "zzqqa2"]
        )
        self.assertEqual(
            list(self.out_json("json/chat_with_xxxxx-2.json")), ["key_1", "key_2", "key_3"]
        )

    def test_a_rewritten_name_is_printed_so_the_vocabulary_can_be_fixed(self):
        self.write_json("login_history.json", {"a": 1})
        self.run_tool()
        self.assertIn("json/xxxxx_history.json", self.stdout)
        self.assertIn("NAME_VOCABULARY", self.stdout)

    def test_an_export_root_of_schema_labels_raises_no_advisory(self):
        self.write_json(
            "account.json",
            {
                "Basic Information": {"a": 1},
                "Device Information": {"a": 1},
                "Frequent Locations": {"a": 1},
                "Latest Location": {"a": 1},
            },
        )
        self.run_tool()
        self.assertNotIn("advisory:", self.stdout)

    def test_a_handle_past_the_counter_width_is_still_accounted(self):
        ctx = rx.Ctx(
            seed=1,
            shift=timedelta(days=-1),
            array_sample=5,
            max_alnum_run=20,
            mask_rules=(),
            stats=rx.Stats(),
            handles=rx.Handles(1),
        )
        ctx.handles.counters["user"] = 100000
        out = rx.synth_join("zzqqusernamemarker", ctx, "user")
        self.assertEqual(out, "user_100001")
        self.assertTrue(rx.token_accounted("100001", ctx.stats))


class TestErrorPaths(RedactorCase):
    def test_missing_src(self):
        self.src = self.root / "nope"
        self.run_tool(expect=rx.EXIT_CONFIG)
        self.assertIn("is not a directory", self.stderr)

    def test_src_without_json_is_refused(self):
        (self.src / "json" / "keep.txt").write_text("x", encoding="utf-8")
        self.run_tool(expect=rx.EXIT_CONFIG)
        self.assertIn("no *.json found", self.stderr)

    def test_dst_inside_src_is_refused(self):
        self.write_json("data.json", {"a": 1})
        self.run_tool(dst=self.src / "out", expect=rx.EXIT_CONFIG)
        self.assertIn("nested", self.stderr)

    def test_dst_equal_to_src_is_refused(self):
        self.write_json("data.json", {"a": 1})
        self.run_tool(dst=self.src, expect=rx.EXIT_CONFIG)
        self.assertIn("same directory", self.stderr)

    def test_dst_that_is_a_file_is_refused_with_a_message(self):
        self.write_json("data.json", {"a": 1})
        target = self.root / "afile"
        target.write_text("x", encoding="utf-8")
        self.run_tool(dst=target, expect=rx.EXIT_CONFIG)
        self.assertIn("is not a directory", self.stderr)

    def test_non_empty_dst_needs_force(self):
        self.write_json("data.json", {"a": 1})
        self.dst.mkdir()
        (self.dst / "old.json").write_text("{}", encoding="utf-8")
        self.run_tool(expect=rx.EXIT_CONFIG)
        self.assertIn("--force", self.stderr)
        self.run_tool("--force")

    def test_a_symlink_in_dst_is_replaced_not_written_through(self):
        self.write_json("data.json", {"a": 1})
        outside = self.root / "outside.json"
        outside.write_text("KEEPME", encoding="utf-8")
        (self.dst / "json").mkdir(parents=True)
        (self.dst / "json" / "data.json").symlink_to(outside)
        self.run_tool("--force")
        self.assertEqual(outside.read_text(encoding="utf-8"), "KEEPME")
        self.assertFalse((self.dst / "json" / "data.json").is_symlink())

    def test_invalid_json_names_the_file_and_the_fix(self):
        self.write_raw("broken.json", "{not json")
        self.run_tool(expect=rx.EXIT_CONFIG)
        # Masked by default, with one documented step to identify the file.
        self.assertIn("json/xxxxxx.json is not valid json", self.stderr)
        self.assertIn("--show-source-names", self.stderr)
        self.assertIn("re-run", self.stderr)

    def test_the_documented_abort_contract_is_what_an_abort_actually_prints(self):
        """The --help paragraph went stale once when the default flipped, and no
        test noticed. Pin the claim to the behaviour in both directions."""
        self.assertIn("they name the MIRRORED file", rx.__doc__)
        self.assertIn("--show-source-names", rx.__doc__)
        self.write_raw("broken.json", "{not json")
        self.run_tool(expect=rx.EXIT_CONFIG)
        self.assertIn("json/xxxxxx.json is not valid json", self.stderr)
        self.assertNotIn("broken", self.stderr)

    def test_show_source_names_names_the_real_file_on_an_abort(self):
        self.write_raw("broken.json", "{not json")
        self.run_tool("--show-source-names", expect=rx.EXIT_CONFIG)
        self.assertIn("json/broken.json is not valid json", self.stderr)
        self.assertNotIn("--show-source-names", self.stderr)  # no hint once it is on

    def test_the_flag_warns_against_pasting_its_output(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            with self.assertRaises(SystemExit):
                rx.main(["--help"])
        self.assertIn("DO NOT paste that output to an assistant", out.getvalue())

    @unittest.skipIf(os.geteuid() == 0, "root bypasses the unreadable-file mode")
    def test_an_unreadable_file_leaks_neither_its_name_nor_its_absolute_path(self):
        target = self.src / "json" / "chat_with_zzqqusernamemarker.json"
        target.write_text("{}", encoding="utf-8")
        target.chmod(0o000)
        self.addCleanup(target.chmod, 0o644)
        self.run_tool(expect=rx.EXIT_CONFIG)
        # str(OSError) would carry the absolute source path next to the masked name.
        self.assertIn("cannot read json/chat_with_xxxxxxxxxxxx.json", self.stderr)
        self.assertIn("Permission denied", self.stderr)
        self.assertNotIn("zzqqusernamemarker", self.stderr)
        self.assertNotIn(str(self.src), self.stderr)
        self.run_tool("--show-source-names", expect=rx.EXIT_CONFIG)
        self.assertIn("cannot read json/chat_with_zzqqusernamemarker.json", self.stderr)
        self.assertNotIn(str(self.src), self.stderr)  # never the absolute path, either way

    def test_non_utf8_json_is_a_named_error_not_a_traceback(self):
        (self.src / "json" / "bad.json").write_bytes(b'{"a": "\xff\xfe"}')
        self.run_tool(expect=rx.EXIT_CONFIG)
        self.assertIn("json/xxx.json", self.stderr)  # mirrored name by default
        self.assertIn("not utf-8", self.stderr)

    def test_duplicate_keys_are_refused_rather_than_silently_collapsed(self):
        self.write_raw("dup.json", '{"zzqqusernamemarker": "first", "zzqqusernamemarker": "x"}')
        self.run_tool(expect=rx.EXIT_CONFIG)
        self.assertIn("duplicate json key", self.stderr)
        self.assertNotIn("zzqqusernamemarker", self.stderr)  # the key stays masked by default
        self.run_tool("--show-source-names", expect=rx.EXIT_CONFIG)
        self.assertIn("json/dup.json", self.stderr)  # source name, so it can be repaired
        self.assertIn("zzqqusernamemarker", self.stderr)  # and the key, behind the flag

    def test_zero_date_shift_is_refused(self):
        self.write_json("data.json", {"a": 1})
        self.run_tool(expect=rx.EXIT_CONFIG, shift="0")
        self.assertIn("zero shift", self.stderr)

    def test_max_alnum_run_below_the_floor_is_refused(self):
        self.write_json("data.json", {"a": 1})
        self.run_tool("--max-alnum-run", "8", expect=rx.EXIT_CONFIG)
        self.assertIn(
            f"must be between {rx.MIN_ALNUM_RUN} and {rx.MAX_ALNUM_RUN_CEILING}", self.stderr
        )

    def test_max_alnum_run_above_the_ceiling_is_refused(self):
        self.write_json("data.json", {"a": 1})
        self.run_tool("--max-alnum-run", "999999", expect=rx.EXIT_CONFIG)
        self.assertIn("the rule stops catching signed blobs", self.stderr)

    def test_negative_array_sample_is_refused(self):
        self.write_json("data.json", {"a": 1})
        self.run_tool("--array-sample", "-1", expect=rx.EXIT_CONFIG)
        self.assertIn("--array-sample", self.stderr)

    def test_a_symlinked_source_dir_is_reported_as_skipped(self):
        self.write_json("data.json", {"a": 1})
        elsewhere = self.root / "elsewhere"
        elsewhere.mkdir()
        (elsewhere / "hidden.json").write_text("{}", encoding="utf-8")
        (self.src / "linked").symlink_to(elsewhere, target_is_directory=True)
        self.run_tool()
        self.assertIn("symlinked dir(s) under src were not walked", self.stdout)
        self.assertFalse((self.dst / "linked").exists())


class TestDeterminism(RedactorCase):
    def payload(self):
        return {
            "Date": "2021-01-15 20:26:14 UTC",
            "Name": "zzqqusernamemarker",
            "Link": "https://app.snapchat.com/a?uid=1",
            "Loc": "Latitude, Longitude: 45.5, -13.25",
            "N": 4211,
        }

    def test_same_seed_gives_byte_identical_output(self):
        self.write_json("data.json", self.payload())
        self.run_tool()
        first = self.all_output_text()
        second_dst = self.root / "again"
        self.run_tool(dst=second_dst)
        self.dst = second_dst
        self.assertEqual(self.all_output_text(), first)

    def test_a_different_seed_gives_different_synthetic_values(self):
        self.write_json("data.json", self.payload())
        self.run_tool()
        first = self.out_json()["Name"]
        self.dst = self.root / "again"
        self.run_tool(seed="99", dst=self.dst)
        self.assertNotEqual(self.out_json()["Name"], first)


class TestHelp(unittest.TestCase):
    def test_help_states_what_it_reads_writes_and_guarantees(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            with self.assertRaises(SystemExit) as raised:
                rx.main(["--help"])
        self.assertEqual(raised.exception.code, 0)
        text = out.getvalue()
        for heading in ("READS", "WRITES", "PRINTS", "GUARANTEES", "NOT GUARANTEED"):
            self.assertIn(heading, text)
        self.assertIn("--mask-keys-under", text)
        self.assertIn("--array-sample", text)

    def test_help_warns_about_every_channel_the_tool_cannot_close(self):
        for promise in (
            "pass through verbatim",
            "BEST EFFORT",
            "exact real counts",
            "recover it and invert",
        ):
            self.assertIn(promise, rx.__doc__)


class TestUnitHelpers(unittest.TestCase):
    def test_tokenize_name_classifies_every_part(self):
        self.assertEqual(
            rx.tokenize_name(f"2021-01-15_media~zip-{UUID_IN}"),
            [
                ("date", "2021-01-15"),
                ("sep", "_"),
                ("literal", "media"),
                ("sep", "~"),
                ("literal", "zip"),
                ("sep", "-"),
                ("uuid", UUID_IN),
            ],
        )

    def test_strip_prefix_is_not_a_char_set_strip(self):
        self.assertEqual(rx.strip_prefix("..jpg", "."), ".jpg")

    def test_shift_epoch_ignores_a_plain_counter(self):
        self.assertIsNone(rx.shift_epoch(4211, 86400))
        self.assertEqual(rx.shift_epoch(1610742374, 86400), 1610742374 + 86400)

    def test_decimals_of_handles_exponent_form(self):
        self.assertIsNone(rx.decimals_of(repr(1e-05)))
        self.assertEqual(rx.decimals_of("12.750"), 3)

    def test_mask_text_stars_digits_and_words_alike(self):
        self.assertEqual(rx.mask_text("40.7128,-74.0060"), "**.****,-**.****")
        self.assertEqual(rx.mask_text("45.5,13.25"), "**.*,**.**")
        self.assertEqual(
            rx.mask_text("https://cdn.sc.test/ab/cde?u=xy"), "*****://***.**.****/**/***?*=**"
        )

    def test_mask_text_keeps_the_redaction_literal_readable(self):
        self.assertEqual(rx.mask_text("a/REDACTED"), "*/REDACTED")

    def test_mask_pointer_stars_names_but_keeps_indices(self):
        self.assertEqual(rx.mask_pointer("/Saved Media/12/Date"), "/***** *****/12/****")

    def test_report_pointer_keeps_only_provably_safe_segments(self):
        self.assertEqual(rx.report_pointer("/zzqquser/0/Media"), "/<key>/0/<key>")
        self.assertEqual(rx.report_pointer("/Saved Media"), "/Saved Media")
        self.assertEqual(rx.report_pointer("/user_7/12"), "/user_7/12")

    def test_token_accounted_covers_mask_runs_and_static_words(self):
        stats = rx.Stats()
        self.assertTrue(rx.token_accounted("xxxxxxxxxxxx", stats))
        self.assertTrue(rx.token_accounted("000000000000", stats))
        self.assertTrue(rx.token_accounted("overlay", stats))
        self.assertFalse(rx.token_accounted("zzqqmarker", stats))

    def test_synthetic_filename_words_stay_under_the_alnum_ceiling(self):
        self.assertLess(rx.MAX_SYNTH_WORD, rx.MIN_ALNUM_RUN)
        self.assertTrue(rx.MASK_RUN_RE.fullmatch("x" * rx.MAX_SYNTH_WORD))

    def test_url_problem_accepts_only_the_sanctioned_query_shape(self):
        base = "https://h.test/REDACTED"
        self.assertIsNone(rx.url_problem(f"{base}?uid=REDACTED&sid=REDACTED"))
        self.assertIsNone(rx.url_problem(f"{base}?REDACTED"))
        self.assertEqual(rx.url_problem(f"{base}?uid=x"), "a parameter payload")
        self.assertEqual(rx.url_problem(f"{base}?bare"), "a parameter payload")

    def test_matching_rules_need_the_same_depth(self):
        rule = rx.parse_mask_rule("data.json:/a")
        self.assertEqual(rx.matching_rules([rule], ("json/data.json",), "/a"), [rule])
        self.assertEqual(rx.matching_rules([rule], ("json/data.json",), "/a/b"), [])
        self.assertEqual(rx.matching_rules([rule], ("json/other.json",), "/a"), [])

    def test_matching_rules_accept_either_mirrored_name(self):
        family = rx.parse_mask_rule("x.json:/")
        one = rx.parse_mask_rule("x-2.json:/")
        aliases = ("json/x.json", "json/x-2.json")
        self.assertEqual(rx.matching_rules([family], aliases, ""), [family])
        self.assertEqual(rx.matching_rules([one], aliases, ""), [one])
        self.assertEqual(rx.matching_rules([one], ("json/x.json", "json/x.json"), ""), [])

    def test_suggest_rule_wildcards_everything_but_schema_labels(self):
        container = {"file": "json/chat_history.json", "pointer": "/user_1/0/Saved Media"}
        self.assertEqual(rx.suggest_rule(container), "chat_history.json:/*/*/Saved Media")
        self.assertEqual(
            rx.suggest_rule({"file": "json/x.json", "pointer": "/zzqqancestor"}), "x.json:/*"
        )

    def test_looks_id_keyed_ignores_repeated_schema_keys(self):
        repeated = Counter({"From": 3, "Content": 3})
        once = Counter({"zzqquser": 1, "zzqqother": 1})
        self.assertFalse(rx.looks_id_keyed({"key_names": ("From", "Content")}, repeated))
        self.assertTrue(rx.looks_id_keyed({"key_names": ("zzqquser", "zzqqother")}, once))

    def test_mask_name_keeps_vocabulary_words_and_masks_the_rest(self):
        self.assertEqual(rx.mask_name("memories_history.json"), "memories_history.json")
        self.assertEqual(
            rx.mask_name("chat_with_zzqqusernamemarker.json"), "chat_with_xxxxxxxxxxxx.json"
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
