# Experimental Rust matcher

This branch integrates a Rust matcher into the existing grep CLI for review.
The default build still uses Oniguruma. Enabling `rust-matcher` switches
`-G`, `-E`, and `-F` to the experimental backend; enabling `onig` alongside it
retains the existing `-P` implementation. This is an initial integration, not a
proposal to change the default or a complete GNU compatibility claim.

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
   `MatchKind::All` DFA to choose the longest match at that start. Both passes
   keep the original haystack so assertions retain their context.
3. Patterns requiring backreferences or unsupported DFA assertions, and
   patterns exceeding the DFA construction limit, use `fancy-regex` 0.19.2.
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
- **Fallback input scope:** the regular path searches arbitrary byte strings.
  Stock fancy-regex has known malformed-UTF-8 VM bugs and a C-mode non-ASCII
  backreference bug. Before entering that fallback, this branch rejects invalid
  UTF-8 in the Unicode profile and non-ASCII input in the C profile with status
  2. This avoids exposing those known wrong answers or panics. The separate
  experimental engine patch has not been incorporated or published here.
- **Resource scope:** the DFA has an 8 MiB size/construction cap. The fallback
  has a 1,000,000-backtrack limit. `(a|aa)+\1` on 28 `a` bytes exhausts that
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

This branch deliberately uses released engine crates, with no vendored fork.
Removing these restrictions is follow-on work before considering a default
switch. No devenv package, registry, or production configuration is changed.

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
switch. End-to-end performance and Linux qualification remain outstanding.
