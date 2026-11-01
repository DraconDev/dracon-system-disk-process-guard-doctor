#!/usr/bin/env python3
"""One-off pure-tracker regression qualification; never mutates live source.

Full Cargo tests remain primary. Extract actual tracker/type/method bodies and
actual fixture checks into a std-only harness. Only the large GuardPolicy is
adapted to its relevant fields, with defaults extracted from production. This
is not a deployment, a collected test, or a permanent source-shape requirement.
"""
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile

OUT = Path(__file__).resolve().parent
ROOT = next(p for p in OUT.parents if (p / "src/service_growth.rs").is_file())
SOURCE = (ROOT / "src/service_growth.rs").read_text()
TESTS = (ROOT / "src/service_growth_tests.rs").read_text()
POLICY = (ROOT / "src/policy.rs").read_text()
WORK = Path(tempfile.mkdtemp(prefix="dracon-growth-qualification-"))


def function(source, name):
    match = re.search(rf"^(?:pub\(crate\) )?fn {name}\b", source, re.M)
    if not match:
        raise ValueError(name)
    opening = source.index("{", match.start())
    depth = 0
    for pos in range(opening, len(source)):
        depth += (source[pos] == "{") - (source[pos] == "}")
        if depth == 0:
            return source[match.start():pos + 1]
    raise ValueError("unbalanced extraction")


pure = SOURCE[SOURCE.index("#[derive("):SOURCE.index("trait Probe")]
pure = re.sub(r"^\s*#\[serde[^\n]*\]\n", "", pure, flags=re.M)
pure = re.sub(r"\b(?:Serialize|Deserialize)\b,?\s*", "", pure)
constants = []
for name in ["MAX_TARGETS", "MAX_HISTORY", "MIB", "PROVENANCE"]:
    constants.append(re.search(rf"^(?:pub\(crate\) )?const {name}: [^\n]*;$", SOURCE, re.M)[0])
prefix = """#![allow(dead_code, unused_imports, unused_variables)]
use std::{collections::{HashMap, BTreeMap, VecDeque}, time::{Instant, Duration}};
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcessIdentity { comm: String, starttime: u64 }
#[derive(Debug, Clone)]
struct GuardPolicy {
 monitor_service_memory_growth: bool,
 service_memory_growth_window_secs: u64,
 service_memory_growth_min_mb: u64,
 service_memory_growth_targets: Vec<String>,
}
impl Default for GuardPolicy {
 fn default() -> Self { Self {
 monitor_service_memory_growth: default_true(),
 service_memory_growth_window_secs: default_service_memory_growth_window_secs(),
 service_memory_growth_min_mb: default_service_memory_growth_min_mb(),
 service_memory_growth_targets: default_targets(),
 } }
}
"""
prefix += "\n".join(constants) + "\n"
prefix += "\n".join(function(POLICY, n) for n in ["default_true", "default_service_memory_growth_window_secs", "default_service_memory_growth_min_mb"]) + "\n"
helpers = "\n".join(function(TESTS, n) for n in ["guard", "observation", "at"])
checks = "\n".join("#[test]\n" + function(TESTS, n) for n in [
 "a_single_burst_plateau_or_resident_swap_migration_is_not_sustained_growth",
 "swap_only_growth_and_irregular_intervals_use_elapsed_time_not_poll_count",
 "service_and_process_generation_changes_never_bridge_history",
 "history_and_event_signatures_stay_bounded_and_quiet",
 "equivalent_target_reordering_preserves_observed_growth",
])
noisy = re.sub(r"middle\.total\.saturating_sub\(baseline\.total\)\s*>= meaningful_half\s*&& total\.saturating_sub\(middle\.total\)\s*>= meaningful_half",
              "middle.total > baseline.total && total > middle.total", pure)
no_identity = pure.replace("if h.generation != generation", "if false")
assert noisy != pure and no_identity != pure, "review extraction/mutation sites after source changes"
results = []
for label, body, failure in [
 ("baseline", pure, None),
 ("negligible-drift", noisy, "a_single_burst_plateau_or_resident_swap_migration_is_not_sustained_growth"),
 ("bridged-incarnation", no_identity, "service_and_process_generation_changes_never_bridge_history"),
]:
    generated = prefix + body + "\n" + helpers + "\n" + checks
    path = WORK / (label + ".rs"); path.write_text(generated)
    binary = WORK / label
    built = subprocess.run(["rustc", "--edition=2021", "--test", str(path), "-o", str(binary)], capture_output=True, timeout=60)
    (OUT / (label + ".compile.log")).write_bytes(built.stdout + built.stderr)
    assert built.returncode == 0, f"{label}: compile failure is not regression evidence"
    ran = subprocess.run([str(binary)], capture_output=True, timeout=30)
    output = ran.stdout + ran.stderr
    (OUT / (label + ".log")).write_bytes(output)
    if failure:
        assert ran.returncode == 101 and (failure + " ... FAILED").encode() in output
    else:
        assert ran.returncode == 0
    results.append(dict(variant=label, compile_exit=built.returncode, test_exit=ran.returncode,
                        expected_failure=failure, generated_source_sha256=hashlib.sha256(generated.encode()).hexdigest()))
(OUT / "qualification.json").write_text(json.dumps(dict(source_sha256=hashlib.sha256(SOURCE.encode()).hexdigest(), tests_sha256=hashlib.sha256(TESTS.encode()).hexdigest(),
                                                      scratch_path=str(WORK), results=results), indent=2) + "\n")
print("PASS: real tracker baseline/reordering pass; compiled noisy-burst and bridged-incarnation mutants fail intended behavioral checks")
