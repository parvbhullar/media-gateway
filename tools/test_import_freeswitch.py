"""Unit tests for tools/import_freeswitch.py Refiner.

Run: python3 -m unittest tools.test_import_freeswitch -v
or:  python3 tools/test_import_freeswitch.py
"""
import json
import os
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
sys.path.insert(0, HERE)

from import_freeswitch import Refiner, RefinedPlan  # noqa: E402


def load_export() -> dict:
    with open(os.path.join(REPO, "freeswitch_export.json")) as f:
        return json.load(f)


class RefinerSmokeTest(unittest.TestCase):
    def test_refine_returns_plan(self):
        plan = Refiner().refine(load_export())
        self.assertIsInstance(plan, RefinedPlan)


from import_freeswitch import expand_fs_pattern  # noqa: E402


class PatternExpanderTest(unittest.TestCase):
    def test_plain_alternation(self):
        self.assertEqual(
            expand_fs_pattern("^(?:00910?|0)(807153913[267])$"),
            ["8071539132", "8071539136", "8071539137"],
        )

    def test_single_literal(self):
        self.assertEqual(
            expand_fs_pattern("^(?:\\+?91|00910?|0)(8071539178)$"),
            ["8071539178"],
        )

    def test_range_class(self):
        self.assertEqual(
            expand_fs_pattern("^(?:00910?|0)(807153910[0-5])$"),
            ["8071539100", "8071539101", "8071539102",
             "8071539103", "8071539104", "8071539105"],
        )

    def test_digit_repetition(self):
        result = expand_fs_pattern("^(?:00910?|0)(8071539[23][0-9]{2})$")
        self.assertEqual(len(result), 2 * 10 * 10)  # 200 numbers
        self.assertIn("8071539200", result)
        self.assertIn("8071539399", result)

    def test_mixed_class_with_two_ranges(self):
        result = expand_fs_pattern("^(?:00910?|0)(807153915[0-24-6])$")
        # [0-24-6] = 0,1,2,4,5,6
        self.assertEqual(result, [
            "8071539150", "8071539151", "8071539152",
            "8071539154", "8071539155", "8071539156",
        ])

    def test_big_real_pattern(self):
        expr = (r"^(?:\+91|0)?(807153910[0-5]|8071539111|807153913[2-9]"
                r"|80715391[4-9][0-9]|807153916[7-9]|8071539[23][0-9]{2})$")
        result = expand_fs_pattern(expr)
        self.assertIn("8071539100", result)
        self.assertIn("8071539111", result)
        self.assertIn("8071539132", result)
        self.assertIn("8071539200", result)
        self.assertEqual(len(result), len(set(result)))  # no duplicates

    def test_unsupported_raises(self):
        with self.assertRaises(ValueError):
            expand_fs_pattern("^(?:00910?|0)(\\d+)$")


class RefineTrunksTest(unittest.TestCase):
    def setUp(self):
        self.plan = Refiner().refine(load_export())

    def test_trunk_count_is_eleven(self):
        self.assertEqual(len(self.plan.trunks), 11)

    def test_orphan_clouds_dropped(self):
        names = {t.name for t in self.plan.trunks}
        for orphan in ("cloud", "cloud_2", "cloud_rustpbx", "cloud_libresbc"):
            self.assertNotIn(orphan, names)
        dropped = {n for n, _ in self.plan.dropped_trunks}
        self.assertTrue({"cloud", "cloud_2", "cloud_rustpbx", "cloud_libresbc"}.issubset(dropped))

    def test_sheemish_dropped_for_placeholder_host(self):
        names = {t.name for t in self.plan.trunks}
        self.assertNotIn("sheemish", names)
        reasons = dict(self.plan.dropped_trunks)
        self.assertIn("sheemish", reasons)
        self.assertIn("placeholder", reasons["sheemish"].lower())

    def test_voda_intphony_has_credentials(self):
        voda = next(t for t in self.plan.trunks if t.name == "voda_intphony")
        self.assertEqual(voda.auth_username, "8071539100")
        self.assertEqual(voda.auth_password, "1234")
        self.assertEqual(voda.sip_server, "sip:10.230.73.220:5060")
        self.assertEqual(voda.sip_transport, "udp")

    def test_livekit_transport_stripped_from_host(self):
        lk = next(t for t in self.plan.trunks if t.name == "livekit")
        self.assertEqual(lk.sip_server, "sip:3i5bvr312d9.sip.livekit.cloud:5060")
        self.assertEqual(lk.sip_transport, "tcp")

    def test_tcp_trunk(self):
        fabriq = next(t for t in self.plan.trunks if t.name == "fabriq")
        self.assertEqual(fabriq.sip_transport, "tcp")
        self.assertEqual(fabriq.sip_server, "sip:15j2dl095m2.sip.livekit.cloud:5060")

    def test_all_bidirectional(self):
        for t in self.plan.trunks:
            self.assertEqual(t.direction, "bidirectional")
            self.assertTrue(t.is_active)
            self.assertFalse(t.register_enabled)


class RefineDidsTest(unittest.TestCase):
    def setUp(self):
        self.plan = Refiner().refine(load_export())

    def test_one_bulk_per_source_row(self):
        self.assertEqual(len(self.plan.dids), 10)

    def test_all_owned_by_voda_intphony(self):
        for bulk in self.plan.dids:
            self.assertEqual(bulk.trunk_name, "voda_intphony")
            self.assertTrue(bulk.enabled)
            self.assertGreater(len(bulk.numbers), 0)

    def test_numbers_are_e164(self):
        for bulk in self.plan.dids:
            for num in bulk.numbers:
                self.assertTrue(num.startswith("+91"))
                self.assertTrue(num[1:].isdigit())
                self.assertEqual(len(num), 13)  # +91 + 10 digits

    def test_fabriq_expansion(self):
        fabriq = next(b for b in self.plan.dids if "fabriq" in b.label)
        self.assertEqual(sorted(fabriq.numbers),
                         ["+918071539132", "+918071539136", "+918071539137"])

    def test_livekit_is_the_largest(self):
        livekit = next(b for b in self.plan.dids if "livekit" in b.label)
        self.assertGreater(len(livekit.numbers), 100)

    def test_label_references_source_route(self):
        for bulk in self.plan.dids:
            self.assertIn("imported:", bulk.label)
            self.assertIn("voda_to_", bulk.label)


class RefineRoutesTest(unittest.TestCase):
    def setUp(self):
        self.plan = Refiner().refine(load_export())

    def test_kept_route_count(self):
        # 24 total - public_did - cloud_to_voda - vapi_to_voda
        # - voda_to_digipanda (empty bridge) - digipanda_to_voda (empty bridge)
        # - voda_to_sheemish (target trunk dropped) = 18
        self.assertEqual(len(self.plan.routes), 18)

    def test_voda_to_sheemish_dropped_for_dropped_trunk(self):
        names = {r.name for r in self.plan.routes}
        self.assertNotIn("voda_to_sheemish", names)
        dropped = dict(self.plan.dropped_routes)
        self.assertIn("voda_to_sheemish", dropped)
        self.assertIn("sheemish", dropped["voda_to_sheemish"])

    def test_public_did_dropped(self):
        names = {r.name for r in self.plan.routes}
        self.assertNotIn("public_did", names)
        dropped = dict(self.plan.dropped_routes)
        self.assertIn("public_did", dropped)

    def test_network_addr_routes_dropped(self):
        names = {r.name for r in self.plan.routes}
        self.assertNotIn("cloud_to_voda", names)
        self.assertNotIn("vapi_to_voda", names)
        dropped = dict(self.plan.dropped_routes)
        self.assertIn("network_addr", dropped["cloud_to_voda"])

    def test_empty_bridge_routes_dropped(self):
        names = {r.name for r in self.plan.routes}
        self.assertNotIn("voda_to_digipanda", names)
        self.assertNotIn("digipanda_to_voda", names)

    def test_voda_to_fabriq(self):
        r = next(r for r in self.plan.routes if r.name == "voda_to_fabriq")
        self.assertEqual(r.direction, "inbound")
        self.assertEqual(r.source_trunk, "voda_intphony")
        self.assertIn("to.user", r.match)
        self.assertEqual(r.match["to.user"], "^(?:00910?|0)(807153913[267])$")
        self.assertEqual(r.rewrite, {"to.user": "+91{1}"})
        self.assertEqual(r.action["select"], "rr")
        self.assertEqual(r.action["target_type"], "sip_trunk")
        self.assertEqual(r.action["trunks"], [{"name": "fabriq", "weight": 1}])

    def test_fabriq_to_voda(self):
        r = next(r for r in self.plan.routes if r.name == "fabriq_to_voda")
        self.assertEqual(r.direction, "outbound")
        self.assertEqual(r.source_trunk, "fabriq")
        self.assertIn("from.user", r.match)
        # to.user=+91{1} from bridge target; no from.user rewrite because
        # FS uses ${caller_id_number} (pass-through variable, not a backref)
        self.assertEqual(r.rewrite, {"to.user": "+91{1}"})
        self.assertEqual(r.action["trunks"], [{"name": "voda_intphony", "weight": 1}])

    def test_livekit_to_voda_has_from_rewrite(self):
        # livekit_to_voda: to.user template is ${effective_destination_number}
        # (FS variable → no to.user rewrite), but sip_from_user=$1 in raw
        # actions → from.user: "{1}" to strip +91/0 prefix
        r = next(r for r in self.plan.routes if r.name == "livekit_to_voda")
        self.assertEqual(r.rewrite, {"from.user": "{1}"})

    def test_sheemish_to_voda_has_from_rewrite(self):
        r = next(r for r in self.plan.routes if r.name == "sheemish_to_voda")
        self.assertEqual(r.rewrite, {"from.user": "{1}"})

    def test_all_routes_have_bridge(self):
        for r in self.plan.routes:
            self.assertEqual(r.action["target_type"], "sip_trunk")
            self.assertEqual(len(r.action["trunks"]), 1)


class TranslateRewriteTest(unittest.TestCase):
    def test_dollar_backrefs_become_braces(self):
        self.assertEqual(
            Refiner._translate_rewrite("+91$1"),
            {"to.user": "+91{1}"},
        )

    def test_multiple_backrefs(self):
        self.assertEqual(
            Refiner._translate_rewrite("$1-$2"),
            {"to.user": "{1}-{2}"},
        )

    def test_fs_variable_becomes_empty(self):
        self.assertEqual(Refiner._translate_rewrite("${destination_number}"), {})
        self.assertEqual(Refiner._translate_rewrite("${effective_destination_number}"), {})

    def test_empty_becomes_empty(self):
        self.assertEqual(Refiner._translate_rewrite(""), {})

    def test_plain_literal_passes_through(self):
        self.assertEqual(
            Refiner._translate_rewrite("+918071539100"),
            {"to.user": "+918071539100"},
        )


from import_freeswitch import refined_trunk_to_record, RefinedTrunk  # noqa: E402


class RefinedTrunkToRecordTest(unittest.TestCase):
    """The emit helper produces the Phase 8a `kind` + `kind_config` shape."""

    def _sample(self) -> RefinedTrunk:
        return RefinedTrunk(
            name="voda_intphony",
            display_name="voda_intphony",
            direction="bidirectional",
            sip_server="sip:10.230.73.220:5060",
            sip_transport="udp",
            auth_username="8071539100",
            auth_password="1234",
            register_enabled=False,
            is_active=True,
            description="Imported from FreeSWITCH x.xml on 2026-04-16",
        )

    def test_top_level_shape(self):
        rec = refined_trunk_to_record(self._sample())
        self.assertEqual(rec["name"], "voda_intphony")
        self.assertEqual(rec["kind"], "sip")
        self.assertEqual(rec["direction"], "bidirectional")
        self.assertTrue(rec["is_active"])
        # No SIP fields leak to the top level.
        for forbidden in (
            "sip_server", "sip_transport", "auth_username", "auth_password",
            "register_enabled", "outbound_proxy", "carrier", "did_numbers",
        ):
            self.assertNotIn(forbidden, rec)

    def test_kind_config_holds_sip_fields(self):
        cfg = refined_trunk_to_record(self._sample())["kind_config"]
        self.assertEqual(cfg["sip_server"], "sip:10.230.73.220:5060")
        self.assertEqual(cfg["sip_transport"], "udp")
        self.assertEqual(cfg["auth_username"], "8071539100")
        self.assertEqual(cfg["auth_password"], "1234")
        self.assertFalse(cfg["register_enabled"])

    def test_kind_config_defaults_for_unset_fields(self):
        cfg = refined_trunk_to_record(self._sample())["kind_config"]
        self.assertIsNone(cfg["outbound_proxy"])
        self.assertIsNone(cfg["register_expires"])
        self.assertEqual(cfg["register_extra_headers"], [])
        self.assertFalse(cfg["rewrite_hostport"])
        self.assertEqual(cfg["did_numbers"], [])
        self.assertIsNone(cfg["incoming_from_user_prefix"])
        self.assertIsNone(cfg["incoming_to_user_prefix"])
        self.assertIsNone(cfg["default_route_label"])
        self.assertIsNone(cfg["billing_snapshot"])
        self.assertIsNone(cfg["analytics"])
        self.assertIsNone(cfg["carrier"])

    def test_register_extra_headers_is_list_of_pairs(self):
        # Shape contract: list of [key, value] pairs (preserves order /
        # duplicates), not a dict.
        cfg = refined_trunk_to_record(self._sample())["kind_config"]
        self.assertIsInstance(cfg["register_extra_headers"], list)


import tempfile


class EnvLoaderTest(unittest.TestCase):
    def test_parses_simple_env(self):
        from import_freeswitch import load_env
        with tempfile.NamedTemporaryFile("w", suffix=".env", delete=False) as f:
            f.write("# a comment\n")
            f.write("SUPER_USERNAME=admin\n")
            f.write("SUPER_PASSWORD=Admin__123\n")
            f.write("RUSTPBX_URL=http://1.2.3.4:8080\n")
            path = f.name
        env = load_env(path)
        self.assertEqual(env["SUPER_USERNAME"], "admin")
        self.assertEqual(env["SUPER_PASSWORD"], "Admin__123")
        self.assertEqual(env["RUSTPBX_URL"], "http://1.2.3.4:8080")

    def test_missing_file_raises(self):
        from import_freeswitch import load_env
        with self.assertRaises(FileNotFoundError):
            load_env("/nonexistent/.env.prod")


if __name__ == "__main__":
    unittest.main()
