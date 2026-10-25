# Experimental Rust matcher

The `rust-matcher` Cargo feature enables a Rust backend for basic (`-G`),
extended (`-E`), and fixed-string (`-F`) matching. It is opt-in; the default
build continues to use Oniguruma.

| Cargo configuration | `-G`, `-E`, `-F` | `-P` |
| --- | --- | --- |
| Default features | Oniguruma | Oniguruma |
| `--features rust-matcher` | Rust backend | Oniguruma |
| `--no-default-features --features rust-matcher` | Rust backend | Unsupported |

Build with both backends to retain `-P`, or disable default features to build
without the C regex engine:

```sh
cargo build --locked --release --features rust-matcher
cargo build --locked --release --no-default-features --features rust-matcher
```

The Rust-only configuration also builds for WASI Preview 1:

```sh
rustup target add wasm32-wasip1
cargo build --locked --release --target wasm32-wasip1 \
  --no-default-features --features rust-matcher
```

The backend uses released `regex-automata` 0.4.18, `regex-syntax` 0.8, and
`fancy-regex` 0.19.2 dependencies. Regular expressions use the automata engine;
backreferences and expressions requiring the fallback use a bounded
backtracking engine. BRE/ERE selection retains the leftmost start and longest
match at that start. The Rust backend does not fall back to Oniguruma.

## Supported scope

- Locale selection reads the first nonempty value of `LC_ALL`, `LC_CTYPE`, and
  `LANG`. An unset locale, `C`, or `POSIX` selects byte-oriented ASCII rules.
  `C.UTF-8` and `en_US.UTF-8` select Rust Unicode rules; common spelling variants
  are accepted. Other locale names produce an error.
- Unicode classes, word boundaries, and case folding follow Rust's Unicode
  definitions. Locale-specific collation and full GNU locale behavior are not
  implemented. Collating symbols and equivalence classes are rejected.
- POSIX bracket classes such as `[[:space:]]` use Rust's ASCII definitions,
  including in the Unicode profile.
- Regular matching accepts arbitrary input bytes. The backtracking fallback
  rejects non-ASCII input in the C/POSIX profile and malformed UTF-8 in the
  Unicode profile. These guarded cases produce an error instead of entering
  unsupported engine paths.
- The fallback permits up to 1,000,000 backtracking operations. Reaching
  the limit produces an error with exit status 2. GNU grep may complete
  expressions that exceed this limit.
- A Rust-only build reports an error for `-P`. Enable `onig` alongside
  `rust-matcher` to retain the existing Perl-mode implementation.

Run the feature configurations' tests with:

```sh
cargo test --locked --features rust-matcher
cargo test --locked --no-default-features --features rust-matcher
```
