#!/usr/bin/env python3
"""Compare complete grep invocations on deterministic, locally generated input.

Only Python's standard library is required. GNU grep is an optional black-box
reference; this script does not download or inspect an implementation's source.
"""

import argparse
import datetime
import hashlib
import json
import os
import platform
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def corpus():
    """Own synthetic fixtures, not copied from another grep's test suite."""
    ascii_rows = b"".join(
        (b"prefix needle123 suffix\n" if i % 8 == 0 else b"ordinary text 4567890\n")
        for i in range(400_000)
    )
    return {
        "empty": b"",
        "ascii": ascii_rows,
        "short-lines": b"a123z\nb456x\na789z\n" * 300_000,
        "long-line": b"a" + b"b" * (1024 * 1024) + b"c\n",
        "alternations": b"aa abc abcd aaaa xyz\n" * 50_000,
        "unicode": "Δelta café Привет 12345\n".encode() * 80_000,
        "case-ascii": b"prefix NeEdLe suffix\nordinary text\n" * 200_000,
        "case-unicode": "CAFÉ café Καφές\n".encode() * 80_000,
        "backrefs": b"abc:abc xyz:xyz a:b\n" * 10_000,
        "backref-budget": b"a" * 28 + b"\n",
        "backref-absent": b"x" * 1024 + b"\n",
        "high-bytes": bytes([0xE2, 0x82, 0x62, 0x0A]) * 10_000,
        "guarded-backref": "é abab\n".encode(),
    }


def cases():
    # -a avoids conflating regex semantics with existing binary-output policy.
    # -c suppresses output volume on throughput cases; -o deliberately exercises
    # longest-span selection, which a line-presence-only workload would miss.
    complex_pattern = "(" + "|".join(f"branch{i:03d}[0-9]{{2,5}}" for i in range(128)) + ")+"
    return [
        ("startup-simple", "empty", "C", ["-aE", "needle"]),
        ("startup-complex", "empty", "C", ["-aE", complex_pattern]),
        ("startup-unicode", "empty", "C.UTF-8", ["-aE", r"\w{3,12}_(a|b)"]),
        ("ascii-match-count", "ascii", "C", ["-aEc", r"needle[0-9]+"]),
        ("ascii-bre-count", "ascii", "C", ["-aGc", r"needle[0-9]\{3\}"]),
        ("ascii-miss-count", "ascii", "C", ["-aEc", r"absent[0-9]+"]),
        ("short-lines-count", "short-lines", "C", ["-aEc", r"^a[0-9]{3}z$"]),
        ("long-line-only-matching", "long-line", "C", ["-aEo", r"ab+c"]),
        ("alternation-only-matching", "alternations", "C", ["-aEo", r"a|aa|abc|abcd"]),
        ("unicode-word-count", "unicode", "C.UTF-8", ["-aEc", r"\w+"]),
        ("unicode-word-only-matching", "unicode", "C.UTF-8", ["-aEo", r"\w+"]),
        ("ascii-ignore-case-count", "case-ascii", "C", ["-aEic", "needle"]),
        ("unicode-ignore-case-count", "case-unicode", "C.UTF-8", ["-aEic", "café"]),
        ("fixed-count", "ascii", "C", ["-aFc", "needle123"]),
        ("backref-success-count", "backrefs", "C", ["-aEc", r"([a-z]+):\1"]),
        ("backref-success-only-matching", "backrefs", "C", ["-aEo", r"([a-z]+):\1"]),
        ("backref-ambiguous-budget", "backref-budget", "C", ["-aEo", r"(a|aa)+\1"]),
        ("backref-absent", "backref-absent", "C", ["-aEo", r"(ab)+\1"]),
        ("byte-class-count", "high-bytes", "C", ["-aEc", "[[:alpha:]]"]),
        ("backref-high-byte-guard", "guarded-backref", "C", ["-aEo", r"(ab)\1"]),
    ]


def invoke(binary, args, data_path, locale, timeout, capture):
    env = os.environ.copy()
    env.update(LC_ALL=locale, LANG=locale, TZ="UTC", GREP_COLORS="")
    env.pop("GREP_OPTIONS", None)
    env.pop("POSIXLY_CORRECT", None)
    start = time.perf_counter_ns()
    try:
        child = subprocess.run(
            ["grep", *args, str(data_path)],
            executable=str(binary),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE if capture else subprocess.DEVNULL,
            # Keep one pipe open even during timing: communicate() can wait for
            # EOF instead of Popen.wait(timeout)'s polling/backoff adding a
            # quantized delay to fast process measurements. Timed stderr is
            # collected and discarded; successful cases emit none.
            stderr=subprocess.PIPE,
            env=env,
            timeout=timeout,
            check=False,
        )
        elapsed = (time.perf_counter_ns() - start) / 1_000_000
        if not capture:
            return {"milliseconds": elapsed, "status": child.returncode}
        return {
            "status": child.returncode,
            "stdout_bytes": len(child.stdout),
            "stdout_sha256": sha256(child.stdout),
            "stderr_bytes": len(child.stderr),
            "stderr_sha256": sha256(child.stderr),
            # Diagnostics often name a temporary input path. Preserve a bounded
            # readable form, without checking private paths into result files.
            "stderr_preview": child.stderr.decode("utf-8", errors="replace")
            .replace(str(data_path), "{input}")[:500],
        }
    except subprocess.TimeoutExpired:
        return {"timeout": True}


def signature(observation):
    if observation.get("timeout"):
        return None
    return observation["status"], observation["stdout_bytes"], observation["stdout_sha256"]


def command_text(binary, *args):
    return subprocess.run([str(binary), *args], check=True, capture_output=True, text=True).stdout.strip()


def run(options):
    binaries = {"baseline": Path(options.baseline).resolve(), "candidate": Path(options.candidate).resolve()}
    if options.gnu:
        binaries["gnu"] = Path(options.gnu).resolve()
    report = {
        "schema": 1,
        "created_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "source_commit": options.source_commit,
        "platform": {"system": platform.system(), "release": platform.release(), "machine": platform.machine()},
        "logical_cpus": os.cpu_count(),
        "python": platform.python_version(),
        "rustc": options.rustc,
        "harness_sha256": sha256(Path(__file__).read_bytes()),
        "cargo_lock_sha256": sha256((Path(__file__).resolve().parents[2] / "Cargo.lock").read_bytes()),
        "profile": "cargo build --locked --release; default vs --features rust-matcher",
        "measurement": "subprocess wall milliseconds; warm page cache; stdout -> null, stderr collected and discarded; includes process startup, regex compilation, input reading, matching and output generation",
        "rounds": options.rounds,
        "warmups": options.warmups,
        "timeout_seconds": options.timeout,
        "binaries": {
            name: {"sha256": sha256(path.read_bytes()), "bytes": path.stat().st_size, "version": command_text(path, "--version").splitlines()[0]}
            for name, path in binaries.items()
        },
        "cases": [],
    }
    inputs = corpus()
    with tempfile.TemporaryDirectory(prefix="grep-rust-bench-") as directory:
        paths = {}
        for name, data in inputs.items():
            paths[name] = Path(directory) / name
            paths[name].write_bytes(data)
        for case_id, input_id, locale, args in cases():
            print(case_id, file=sys.stderr, flush=True)
            row = {
                "id": case_id,
                "argv": ["grep", *args, "{input}"],
                "locale": locale,
                "input_bytes": len(inputs[input_id]),
                "input_sha256": sha256(inputs[input_id]),
                "validation": {},
                "timing": {},
            }
            for name, binary in binaries.items():
                row["validation"][name] = invoke(binary, args, paths[input_id], locale, options.timeout, True)
            baseline = signature(row["validation"]["baseline"])
            candidate = signature(row["validation"]["candidate"])
            row["baseline_candidate_stdout_status_agree"] = baseline is not None and baseline == candidate
            if "gnu" in binaries:
                gnu = signature(row["validation"]["gnu"])
                row["gnu_stdout_status_agreement"] = {
                    name: gnu is not None and signature(row["validation"][name]) == gnu
                    for name in ("baseline", "candidate")
                }
            # Time only the two builds of the same frontend. The optional GNU
            # executable is a correctness reference. Its output-discarding or
            # early-exit optimizations need not match this frontend's behavior.
            names = ["baseline", "candidate"]
            samples = {name: [] for name in names}
            invalid = {name: [] for name in names}
            # Alternate the order so neither build is always measured first.
            for iteration in range(options.warmups + options.rounds):
                order = names[:]
                if iteration % 2:
                    order.reverse()
                for name in order:
                    if row["validation"][name].get("timeout"):
                        continue
                    sample = invoke(binaries[name], args, paths[input_id], locale, options.timeout, False)
                    if sample.get("timeout") or sample.get("status") != row["validation"][name]["status"]:
                        invalid[name].append(sample)
                    elif iteration >= options.warmups:
                        samples[name].append(sample["milliseconds"])
            for name, values in samples.items():
                if values:
                    row["timing"][name] = {
                        "median_ms": statistics.median(values),
                        "min_ms": min(values),
                        "max_ms": max(values),
                        "samples_ms": values,
                    }
                if invalid[name]:
                    row["timing"].setdefault(name, {})["invalid_runs"] = invalid[name]
            # A wrong answer or budget failure does not earn a speedup ratio.
            if (
                row["baseline_candidate_stdout_status_agree"]
                and (
                    "gnu" not in binaries
                    or all(row["gnu_stdout_status_agreement"].values())
                )
                and row["validation"]["baseline"].get("status") in (0, 1)
                and not any(invalid[name] for name in ("baseline", "candidate"))
                and all(samples[name] for name in ("baseline", "candidate"))
            ):
                row["candidate_over_baseline_median"] = statistics.median(samples["candidate"]) / statistics.median(samples["baseline"])
            report["cases"].append(row)
    options.output.parent.mkdir(parents=True, exist_ok=True)
    options.output.write_text(json.dumps(report, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", required=True, help="Default-Oniguruma release binary")
    parser.add_argument("--candidate", required=True, help="Same source/profile, release binary with rust-matcher enabled")
    parser.add_argument("--gnu", help="Optional GNU grep executable, used only as a black-box reference")
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--rustc", default="not recorded", help="Output of rustc --version --verbose")
    parser.add_argument("--rounds", type=int, default=9)
    parser.add_argument("--warmups", type=int, default=2)
    parser.add_argument("--timeout", type=float, default=20.0)
    options = parser.parse_args()
    if options.rounds < 1 or options.warmups < 0 or options.timeout <= 0:
        parser.error("rounds/timeout must be positive and warmups nonnegative")
    run(options)


if __name__ == "__main__":
    main()
