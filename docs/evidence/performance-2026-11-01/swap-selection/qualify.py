#!/usr/bin/env python3
"""Compile actual sampling/ranking functions in an isolated std-only fixture.

No live source is mutated. The normal Cargo suite remains the primary gate;
this small source-extraction harness proves that its relevant behavioral
checks tolerate reordered input but reject restored early-RSS truncation and
removed incarnation verification. No Cargo runner is invoked from a test.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

OUT = Path(__file__).resolve().parent
ROOT = next(p for p in OUT.parents if (p / "src/main.rs").is_file())
MAIN = (ROOT / "src/main.rs").read_text()
TESTS = (ROOT / "src/memory_sampling_tests.rs").read_text()
WORK = Path(tempfile.mkdtemp(prefix="dracon-swap-qualification-"))


def block(source, pattern):
    match = re.search(pattern, source, re.M)
    if not match:
        raise ValueError(f"source item not found: {pattern}")
    start = source.index("{", match.start())
    depth = 0
    # Selected items contain balanced braces in their format strings/comments;
    # rustc compilation and the original Cargo suite also check the extraction.
    for pos in range(start, len(source)):
        depth += (source[pos] == "{") - (source[pos] == "}")
        if depth == 0:
            return source[match.start():pos + 1]
    raise ValueError("unclosed source item")


def function(source, name):
    return block(source, rf"^(?:pub\(crate\) )?fn {name}\b")


prefix = """#![allow(dead_code)]
use std::{collections::HashSet, fs, io::Read, path::{Path, PathBuf}};
#[derive(Debug, Clone)]
""" + block(MAIN, r"^pub\(crate\) struct ProcSample\b") + "\n"
prefix += "#[derive(Debug, Clone, PartialEq, Eq)]\n" + block(MAIN, r"^pub\(crate\) struct ProcessIdentity\b") + "\n"
for name in ["MAX_PROCESS_STATUS_BYTES", "MEMORY_OFFENDER_LIMIT"]:
    prefix += re.search(rf"^const {name}: .*;$", MAIN, re.M)[0] + "\n"
prefix += re.search(r"const KERNEL_PROCESS_PREFIXES: .*?= &\[.*?\];", MAIN, re.S)[0] + "\n"
prefix += """
mod tempfile {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    pub struct TempDir(PathBuf);
    impl TempDir { pub fn path(&self) -> &Path { &self.0 } }
    pub fn tempdir() -> std::io::Result<TempDir> {
        let path = PathBuf::from(std::env::var_os("QUALIFICATION_FIXTURES").unwrap())
            .join(NEXT.fetch_add(1, Ordering::Relaxed).to_string());
        fs::create_dir_all(&path)?;
        Ok(TempDir(path))
    }
}
"""
functions = {name: function(MAIN, name) for name in [
    "parse_ps_output", "read_proc_identity", "read_proc_swap_mb_from",
    "verified_process_samples_with", "memory_pressure_footprint_mb",
    "memory_pressure_offenders", "is_kernel_process",
]}
helpers = "\n".join(function(TESTS, name) for name in ["row", "identity", "swap", "sample", "pids"])
checks = "\n".join("#[test]\n" + function(TESTS, name) for name in [
    "swapped_offender_outside_rss_shortlist_is_selected",
    "swapping_and_equal_footprints_are_independent_of_input_order",
    "process_churn_across_swap_read_discards_the_sample",
    "swap_parser_rejects_missing_malformed_and_oversized_status",
    "exemptions_and_kernel_threads_never_become_memory_offenders",
])
original = functions["verified_process_samples_with"]
early = original.replace("    parsed\n        .into_iter()", """    let mut parsed = parsed;
    parsed.sort_by_key(|p| std::cmp::Reverse(p.rss_mb));
    parsed.truncate(MEMORY_OFFENDER_LIMIT);
    parsed
        .into_iter()""")
identity = original.replace("""            if read_proc_identity(root, p.pid).ok()? != before {
                return None;
            }""", "            let _ = read_proc_identity(root, p.pid);")
assert early != original and identity != original, "mutation no longer applies; review source"
results = []
for name, sampler, expected_failure in [
    ("baseline", original, None),
    ("early-rss-shortlist", early, "swapped_offender_outside_rss_shortlist_is_selected"),
    ("missing-incarnation-check", identity, "process_churn_across_swap_read_discards_the_sample"),
]:
    sources = dict(functions)
    sources["verified_process_samples_with"] = sampler
    text = prefix + "\n" + "\n".join(sources.values()) + "\n" + helpers + "\n" + checks
    path = WORK / (name + ".rs")
    path.write_text(text)
    binary = WORK / name
    compile_cmd = ["rustc", "--edition=2021", "--test", str(path), "-o", str(binary)]
    compiled = subprocess.run(compile_cmd, capture_output=True, timeout=60)
    (OUT / (name + ".compile.log")).write_bytes(compiled.stdout + compiled.stderr)
    assert compiled.returncode == 0, f"{name}: compilation failure is NOT mutation evidence"
    env = dict(os.environ, QUALIFICATION_FIXTURES=str(WORK / (name + "-fixtures")))
    executed = subprocess.run([str(binary)], env=env, capture_output=True, timeout=30)
    output = executed.stdout + executed.stderr
    (OUT / (name + ".log")).write_bytes(output)
    if expected_failure:
        assert executed.returncode == 101 and (expected_failure + " ... FAILED").encode() in output
    else:
        assert executed.returncode == 0
    results.append(dict(variant=name, compile_exit=compiled.returncode,
                        test_exit=executed.returncode, expected_failure=expected_failure,
                        generated_source_sha256=hashlib.sha256(text.encode()).hexdigest()))
summary = dict(source_main_sha256=hashlib.sha256(MAIN.encode()).hexdigest(),
               source_tests_sha256=hashlib.sha256(TESTS.encode()).hexdigest(),
               scratch_path=str(WORK), results=results)
(OUT / "qualification.json").write_text(json.dumps(summary, indent=2) + "\n")
print("PASS: baseline/reordering pass; compiled early-shortlist and missing-identity mutants fail the intended checks")
