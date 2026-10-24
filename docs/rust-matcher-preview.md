# Experimental Rust matcher

This branch integrates a Rust matcher into the existing grep CLI for review.
The default build still uses Oniguruma. Enabling `rust-matcher` switches
`-G`, `-E`, and `-F` to the experimental backend; enabling `onig` alongside it
retains the existing `-P` implementation. This is an initial integration, not a
proposal to change the default or a complete GNU compatibility claim.

This byte-integration review branch additionally overrides `fancy-regex` with
the pinned commit `e812cbff72bfb087cfd38f5d4e0f9d477c4c17c5` in
[`macovedj/fancy-regex`](https://github.com/macovedj/fancy-regex/commit/e812cbff72bfb087cfd38f5d4e0f9d477c4c17c5).
That commit is based on upstream `d18bf81b6d796d3d60674f3ef4a7d7fa8763bdc0`
plus commits `6c903f7` (case-insensitive byte boundaries) and `e812cbf`
(UnicodeBytes VM traversal and scalar consumption). The Git override is
qualification infrastructure; replace it with a released engine dependency
before proposing this grep integration upstream. Version `0.19.2` alone does
not identify these sources: upstream main and the older published crate both
declare that version. The preceding preview remains on released dependencies.

## Try it

```sh
# Existing backend, unchanged selection policy.
cargo build --locked

# Rust BRE/ERE/fixed matching, existing Oniguruma for -P.
cargo build --locked --features rust-matcher
LC_ALL=C target/debug/grep -Eo 'a|aa' <<<'aa'
LC_ALL=C target/debug/grep -Ebo '(ab)\1' <<<'zabab'

# Entirely Rust matcher dependencies; -P produces a clear unsupported error.
cargo build --locked --no-default-features --features rust-matcher
cargo build --locked --release --no-default-features --features rust-matcher \
  --target wasm32-wasip1
```

The first example prints `aa`; the second prints `1:abab`. Run the two builds
with separate `CARGO_TARGET_DIR` values if you want to keep both binaries.

## Design and review order

1. `src/matcher/gnu_syntax.rs` translates GNU BRE/ERE grammar into engine syntax.
   BRE escaped operators, context-sensitive anchors, intervals, backreferences,
   GNU word/space escapes, and literal bracket characters keep their grep
   meanings. Rust-only syntax must not accidentally become a GNU extension.
2. `src/matcher/rust_backend.rs` tries `regex-automata` 0.4.18 first. It finds
   the earliest match, then enumerates endpoints with an **anchored**
   `MatchKind::All` lazy DFA to choose the longest match at that start. Both
   passes keep the original haystack so assertions retain their context. Syntax
   and NFA construction remain eager; DFA transitions are cached as needed.
3. Patterns requiring backreferences or unsupported DFA assertions, and
   NFAs unable to fit the lazy DFA's minimum cache capacity, use `fancy-regex` 0.19.2.
   A first-match search finds the start; an anchored `leftmost_longest` search
   finds the extent. The whole expression uses this fallback, preserving
   captures and branch relationships.
4. `src/matcher.rs` retains the existing multi-pattern merge and literal buffer
   fast path. Matching is now fallible; `src/searcher.rs` sends runtime errors
   through the existing per-file diagnostic/status-2 path. Backend spans are
   also checked before reaching the output code. This turns two existing
   malformed-byte Oniguruma `-P` output panics into status-2 diagnostics.
5. `tests/test_rust_matcher.rs` exercises these behaviors through the real CLI,
   with child-local locale settings and actual malformed input bytes.

The new dependencies are permissively licensed: regex-automata and regex-syntax
are MIT OR Apache-2.0; fancy-regex is MIT. GNU grep is used only as a black-box
reference executable. No GNU implementation source or regex engine is included.

## Explicit limits

- **Locale scope:** C/POSIX use byte matching. C.UTF-8 and en_US.UTF-8 use a
  generic Unicode profile, with common spelling aliases accepted. Other locales
  produce an unsupported diagnostic. This does not implement libc collation,
  locale-specific case folding, or GNU Unicode character classes. POSIX classes
  retain Rust's ASCII membership; Unicode word classes retain Rust's membership.
- **Byte-engine dependency:** with the pinned engine override above, the
  fallback no longer blanket-rejects C-locale high bytes or malformed UTF-8.
  Search recovery steps over malformed bytes; Unicode consuming dots still
  require valid scalars. C mode retains raw-byte matching. Case-insensitive
  backreferences validate the compared bytes rather than unrelated adjacent
  malformed bytes. This depends on both upstream changes since the published
  release and the local fixes; removing the override is not qualified.
- **Word-boundary policy:** Rust Unicode membership and behavior around
  malformed input still differ from GNU. For example, bare `\b` on bytes
  `E2 28 A1` in C.UTF-8 returns status 1 here but status 0 in GNU despite both
  printing no spans. This is an explicit remaining compatibility difference.
- **Resource scope:** the lazy DFA has an 8 MiB transition-cache capacity;
  this does not bound total regex/NFA memory. A full cache can clear and rebuild,
  so cache churn can increase matching cost. This replaces the original eager
  DFA size/construction caps and can keep more patterns in the regular engine.
  The fallback has a 1,000,000-backtrack limit. `(a|aa)+\1` on 28 `a` bytes exhausts that
  budget in longest selection; long absent backreference searches can also
  exhaust it. These are errors, never silently treated as nonmatches. Increasing
  the limit does not solve exponential ambiguity.
- **Syntax scope:** collating symbols and equivalence classes are explicitly
  unsupported. Unsupported or malformed constructs can have different
  diagnostics from GNU. The syntax adapter needs broader differential testing.
- **Existing command gaps:** `-w` still filters a chosen longest span rather
  than retrying shorter acceptable candidates. File filtering, traversal,
  binary detection, and other existing CLI gaps are outside this integration.
  The retained Oniguruma `-P` path keeps its existing compatibility limitations,
  including longest selection of lazy expressions. A Rust-only build has no
  PCRE replacement.

The pinned engine fork is an experimental dependency until suitable engine
changes are released. Remaining class, word, resource, and command gaps still
need work before considering a default switch. No devenv package, registry,
or production configuration is changed.

## Validation

```sh
cargo fmt --check
cargo test --locked
cargo test --locked --features rust-matcher
cargo test --locked --no-default-features --features rust-matcher
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo clippy --locked --all-targets --no-default-features \
  --features rust-matcher -- -D warnings
```

The general CLI suite gates only the assertions requiring the optional `-P`
backend, so the Rust-only build also runs the rest of the command tests.

Initial validation on 2026-10-23 used macOS arm64 with Rust 1.98.0-nightly
(`31a9463c6`, 2026-05-25), GNU grep 3.12 as a black-box oracle, and Wasmtime
47.0.2. Further platform and minimum-version validation is recorded below;
browser-host qualification remains separate.

| Build | Passing tests |
| --- | ---: |
| Default Oniguruma | 115 |
| Rust matcher plus Oniguruma `-P` | 163 |
| Rust only: unit, general CLI and dedicated CLI tests | 159 |

All three feature configurations pass Clippy with warnings denied; formatting
passes. The Rust-only WASIp1 release build succeeds with only
`wasi_snapshot_preview1` imports and no Oniguruma dependency.

An independent set of 99 targeted GNU probes has 80 stdout/status agreements.
The 19 differences are the documented C fallback high-byte guard (5), UTF-8
fallback guard (6), Unicode POSIX class policy (7), and Unicode word policy (1).
Another 13 branch/backreference probes agree with GNU. Diagnostic text is
retained in the observations but excluded from those agreement counts.

The same 99 targeted cases have identical stdout/status and diagnostic text
between the native Rust-only binary and Wasmtime, after normalizing only the
executable-name prefix in diagnostics. That includes known GNU differences;
host agreement is a separate check from GNU compatibility.

The broader adversarial corpus from the preceding investigation gives:

| Backend | Identical stdout/status/stderr | Different stdout/status | Stderr-only differences |
| --- | ---: | ---: | ---: |
| Baseline main (`9323666684afca5a375dda69ab4a565949891f90`) | 960 | 148 | 51 |
| This branch, Rust plus existing `-P` | 1016 | 92 | 51 |

Of these 1,159 probes, 87 previously different cases now agree and 31 previously
agreeing cases differ. The regressions include Unicode class policies, guarded
fallback input, and a retained `-P` invalid span now diagnosed instead of silently
ignored. Repeated probes overlap bug families, so these counts are **not a GNU
compatibility percentage**. No timeouts or panics occurred in the candidate run.
This evidence supports further work on the adapter and engines, not a default
switch. Whole-command performance and platform results follow below.


## Further qualification

The [feature-specific CI run](https://github.com/macovedj/grep/actions/runs/40010552080)
passed all 15 jobs for commit `e3f53e574706283fb5c0ebe9ab0370a105f369d9`:

- Stable Rust: build, full applicable test suite and Clippy with warnings denied
  on Linux, macOS and Windows for default Oniguruma, both engines, and Rust only.
- Rust 1.88: `cargo check --locked --all-targets` for all three configurations
  on Linux. Local macOS Rust 1.88 execution also passes 115 default, 163 mixed,
  and 159 Rust-only tests, without dependency or lockfile changes.
- Formatting, a Rust-only WASIp1 release build, and a Linux benchmark report.

The general test suite now gates only successful `-P` assertions requiring
Oniguruma. Rust-only builds retain 101 general CLI tests as well as 37 unit and
21 dedicated integration tests. CI exposed races in two existing invalid-option
tests: argument parsing could exit before their stdin writer ran. Those cases
now assert argument failure without an unnecessary competing input writer.

[`benchmarks/rust-matcher`](../benchmarks/rust-matcher/README.md) contains a
portable whole-command benchmark and the macOS results. The
[Linux report](benchmarks/rust-matcher-linux-x86_64.json) is preserved from the
CI artifact for the checked commit. Each report uses 20 deterministic cases,
two warmups and nine alternating timing rounds; GNU is a black-box correctness
reference. Ratios are emitted only for successful equivalent output/status in
both builds and GNU. The three known differing/error cases have no speedup
ratio, and neither platform run has timeouts or invalid timed samples.

Selected final Linux whole-command medians, in milliseconds:

| Case | Oniguruma | Rust | Rust / Oniguruma |
| --- | ---: | ---: | ---: |
| Empty input, bounded Unicode word expression | 0.83 | 67.98 | 81.59 |
| Empty input, 128-branch expression | 1.24 | 21.48 | 17.38 |
| Short-line count, 5.4 MB | 52.67 | 13.86 | 0.26 |
| Repeated alternations with `-o`, 1.05 MB | 54.48 | 13.85 | 0.25 |
| Unicode words with `-o`, 2.56 MB | 87.70 | 35.22 | 0.40 |

The Linux oracle is GNU grep 3.11 and the compiler is Rust 1.99.1; the macOS
oracle is GNU grep 3.12 and its compiler is the recorded 1.98 nightly. Both
platforms have 17 GNU-agreeing benchmark cases and the same three known
exceptions. Their differing machines and compilers make cross-platform timing
ratios uncontrolled comparisons. These are synthetic warm-cache measurements,
with raw samples retained and no overall performance score.

At that checked commit, startup/compilation latency was a substantive
regression despite the scan gains. The local follow-up below addresses much of
that cost. The bounded fallback and Unicode compatibility gaps remain, so these
checks do not establish readiness to replace the default backend.

## Local grammar and startup follow-up

Leading BRE `\+`, `\?`, and `\{` are now literal when there is no preceding
consuming atom, including after assertions and at group/alternative starts.
Malformed leading intervals remain literal. The CLI tests also retain the
bracket, one-digit backreference, repeated-quantifier, and buffer-anchor cases
identified while comparing the adapter with uutils `expr`.

Profiling isolated the bounded Unicode pattern's compilation cost in eager
full determinization. The longest-match automaton now uses regex-automata's
lazy DFA with a reusable cache. A mutex keeps each complete overlapping search
consistent if a compiled pattern is shared between threads. Pattern validation,
the original-haystack anchoring rule, and status-2 runtime errors remain eager
or explicit as appropriate; the implementation does not defer malformed-pattern
diagnostics until input arrives.

The [local macOS report](benchmarks/rust-matcher-lazy-dfa-macos-arm64.json)
compares the retained eager-DFA Rust binary with the updated Rust binary using
the same recorded compiler, lockfile, release profile, and nine alternating
timing rounds after two warmups. This comparison is between two Rust builds;
the earlier reports compare Rust with Oniguruma. The portable harness now adds
four bounded-Unicode cases to its original 20, including nonempty input and
`-o`, to check that the saving persists through actual matching.

| Case | Eager Rust, ms | Lazy Rust, ms |
| --- | ---: | ---: |
| Empty input, bounded Unicode expression | 51.15 | 5.20 |
| Empty input, 128-branch expression | 16.32 | 4.08 |
| Bounded Unicode expression with short matching input, `-o` | 56.77 | 6.36 |
| Bounded Unicode expression over 1.32 MB, `-o` | 63.21 | 14.86 |
| Repeated alternations with `-o`, 1.05 MB | 12.78 | 14.28 |

The startup saving has a throughput tradeoff: the repeated-alternation scan is
about 12% slower in this run, and the long-line `-o` case is about 9% slower.
This local run does not establish new Linux or Windows timings. All 24 cases
retain identical before/after stdout/status. Of those, 22 also agree with GNU;
the two remaining cases are the unchanged ambiguous-backreference budget and
high-byte fallback guard. Wrong/error cases receive no speed ratio. There are
no timeouts or invalid timed samples.

Local checks after the change:

- Default tests: 115 passed; both engines: 169; Rust only: 165.
- Clippy with warnings denied for all three feature configurations; formatting
  and diff checks pass.
- Rust 1.88 checks the Rust-only library and binary without lockfile changes.
- The 1,159-case adversarial corpus retains the same before/after
  stdout/status/stderr classifications; the new grammar cases have dedicated
  CLI regressions. Existing GNU compatibility limits remain.
- The Rust-only WASIp1 release build succeeds. All 99 previous targeted probes
  plus eight grammar/Unicode probes have identical native/Wasmtime stdout,
  stderr, and status. Of these 107 cases, 88 agree with GNU stdout/status;
  the same 19 documented policy/guard differences remain.

These are local results. The CI run above validates the earlier commit and has
not been rerun for this follow-up.

## Local byte-engine qualification

The engine changes are kept in a separate repository and two focused commits.
One fixes case-insensitive backreferences beside malformed bytes; the other
distinguishes candidate advancement from scalar-consuming instructions, with
validated forward/reverse UnicodeBytes traversal. The optimized absent repeater
uses the same scalar consumption as its expanded form. Tests cover seek on/off,
longest on/off, captures and iteration, bounded/anchored searches, valid Unicode,
malformed/truncated encodings, and raw ASCII-mode high bytes.

Current upstream already supplies relevant ASCII/byte fixes, including
[PR 300](https://github.com/fancy-regex/fancy-regex/pull/300). Those are reused,
not reimplemented in the local patch. The new UnicodeBytes recovery regressions
still fail on current upstream: six of the initial nine tests fail, including
a truncated-input panic. The independent case-insensitive regression was also
observed failing before its fix.

Local evidence after applying both engine commits:

- Engine: 727 default tests and 753 with `leftmost_longest`; examples (12),
  playground (37), no-default feature checks, benches, and formatting pass.
- Grep: 170 tests with both engines; 166 Rust-only tests. The backtracking budget
  remains an explicit status-2 error.
- The original 99 targeted probes improve from 80 to 90 GNU stdout/status
  agreements. Nine differences remain in Unicode classes, word membership,
  and malformed-input word-boundary selection.
- A further 97 byte-focused probes improve from 16 to 89 agreements. The eight
  remaining differences are three word policies, two POSIX class policies,
  and three backtracking-budget cases. These selected counts are not an overall
  GNU compatibility percentage.
- The 1,159-case adversarial corpus gains 17 exact GNU agreements and loses
  none: 1,033 exact observations, 75 stdout/status differences, and 51 stderr-only
  differences. Two additional changed observations remain behavior differences.
- All 196 targeted native/Wasmtime observations agree, including known GNU
  differences. This is host parity, not a compatibility claim.

Four affected-path CLI benchmarks also retain GNU output/status agreement on
valid input. Against the preceding guarded preview, local medians are 3.12 vs
3.19 ms for ASCII backreference count, 6.40 vs 6.22 ms for ASCII spans, 10.98 vs
9.57 ms for Unicode spans, and 13.22 vs 12.65 ms for Unicode case-folded spans.
These warm-cache samples include the intervening upstream engine changes;
they do not isolate the cost of either local engine commit or establish a
general speedup. The earlier startup/scan tradeoffs remain relevant.

The earlier benchmark and CI records above apply to their recorded commits.
These byte changes were qualified locally before publishing review branches.
The reviewed engine source is unchanged by replacing the local path override
with its exact Git commit. Updated GitHub CI results must be checked separately;
no upstream PR has been opened.
