"""Protocol and false-pass regressions; no Spike or cross compiler required."""
import copy
import importlib.util
from pathlib import Path
import sys
import unittest

SPEC = importlib.util.spec_from_file_location(
    "difftest", Path(__file__).resolve().parents[1] / "difftest.py")
d = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = d
SPEC.loader.exec_module(d)


def state():
    result = dict.fromkeys(d.FIELDS, 0)
    result.update(pc=0x80000000, priv=3)
    return result


def model_line(frame):
    return "s1 " + " ".join(
        f"{key}={value:d}" if key == "priv" else f"{key}={value:08x}"
        for key, value in frame.items())


def spike_lines(frame):
    return "\n".join({0: "U", 1: "S", 3: "M"}[frame[key]]
                     if key == "priv" else f"0x{frame[key]:08x}"
                     for key in d.FIELDS)


class DifferentialTests(unittest.TestCase):
    def test_independent_state_roundtrip(self):
        frame = state()
        frame.update(x31=0x12345678, c342=2, c343=0xffffffff)
        self.assertEqual(d.parse_model_states(model_line(frame)), [frame])
        self.assertEqual(d.parse_spike(spike_lines(frame), 1), ([frame], []))

    def test_missing_and_duplicate_model_fields_fail(self):
        for text in ("", model_line(state()).replace("x31=00000000", ""),
                     model_line(state()) + " x0=00000000", "s2 pc=0"):
            with self.subTest(text=text), self.assertRaises(d.Failure):
                d.parse_model_states(text)

    def test_truncated_and_extra_spike_reads_fail(self):
        rows = spike_lines(state()).splitlines()
        for text in ("", "\n".join(rows[:-1]), "\n".join(rows + ["0x0"])):
            with self.subTest(text=text), self.assertRaises(d.Failure):
                d.parse_spike(text, 1)

    def test_unwritten_register_corruption_is_detected(self):
        model, reference = state(), state()
        reference["x19"] = 42
        with self.assertRaisesRegex(d.Failure, "x19"):
            d.compare_states([model], [reference])

    def test_intermediate_trap_csr_change_cannot_hide_in_final_state(self):
        model = [state(), state(), state()]
        reference = copy.deepcopy(model)
        reference[1]["c341"] = 0x80000008
        with self.assertRaisesRegex(d.Failure, "step 1, mepc"):
            d.compare_states(model, reference)

    def test_privilege_and_pc_are_compared(self):
        for key, value in (("priv", 1), ("pc", 0x80000004)):
            reference = state()
            reference[key] = value
            with self.subTest(key=key), self.assertRaisesRegex(d.Failure, key):
                d.compare_states([state()], [reference])

    def test_masks_ignore_capability_bits_but_check_implemented_fields(self):
        reference = state()
        reference["c7a1"] = 0x20000000
        d.compare_states([state()], [reference])
        reference["c7a1"] |= 4
        with self.assertRaisesRegex(d.Failure, "tdata1"):
            d.compare_states([state()], [reference])

    def test_initial_pmp_difference_is_not_globally_masked(self):
        reference = state()
        reference["c3a0"] = 0x1f
        with self.assertRaisesRegex(d.Failure, "pmpcfg0"):
            d.compare_states([state()], [reference])

    def test_retired_prefix_cannot_pass_as_full_trace(self):
        event = d.Event(0x80000000, 0x13, {}, [], {})
        with self.assertRaisesRegex(d.Failure, "length differs"):
            d.compare_events([event, event], [event])
        with self.assertRaisesRegex(d.Failure, "state count"):
            d.compare_states([state(), state()], [state()])

    def test_csr_tokens_and_load_store_commits(self):
        event = d.parse_spike_event(
            "core   0: 3 0x80000000 (0x30529073) x5 0x1 "
            "c773_mtvec 0x80000100 mem 0x80001000 mem 0x80001004 0x2")
        self.assertEqual(event.registers, {5: 1})
        self.assertEqual(event.csrs, {0x305: 0x80000100})
        self.assertEqual(event.stores, [(0x80001004, 2)])
        with self.assertRaises(d.Failure):
            d.parse_spike_event("core 0: 3 0x80000000 (0x13) mystery 0x1")

    def test_trace_step_sequence_includes_traps(self):
        events, steps = d.parse_model_trace(
            "v1\ni 1 pc=80000000 inst=00000013\n"
            "t 2 pc=80000004 inst=00000073 cause=11 tval=00000000\n"
            "i 3 pc=80000100 inst=00000013\n")
        self.assertEqual((steps, len(events)), (3, 2))
        for text in ("", "v2", "v1\ni 2 pc=80000000 inst=00000013"):
            with self.subTest(text=text), self.assertRaises(d.Failure):
                d.parse_model_trace(text)

    def test_nonzero_exit_and_timeout_are_failures(self):
        with self.assertRaisesRegex(d.Failure, "exited 7"):
            d.run([sys.executable, "-c", "raise SystemExit(7)"], timeout=5)
        with self.assertRaises(d.Failure):
            d.run([sys.executable, "-c", "import time; time.sleep(10)"], timeout=0.05)

    def test_debugger_is_bounded_and_reads_full_state_after_each_step(self):
        commands = d.debug_commands(3).splitlines()
        self.assertEqual(commands.count("rs 1"), 3)
        self.assertEqual(commands.count("reg 0 31"), 4)
        self.assertEqual(commands.count("reg 0 scause"), 4)
        self.assertEqual(commands[-1], "quit")


if __name__ == "__main__":
    unittest.main()
