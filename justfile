# javak-lsp tasks. `just --list` shows them.

benchmark_test := "integrationtest::benchmark::javak_answers_what_jdtls_answers_on_real_projects"

# Unit tests.
test:
    cargo test

# Every integration case under src/integrationtest/cases.
integration:
    INTEGRATION=1 cargo test --lib integrationtest

#   just benchmark                  every project, evenly spaced questions
#   just benchmark jsoup,javapoet   only those projects
#   just benchmark "" random        random questions; the seed is printed
#   just benchmark jsoup 42         replay the random questions of seed 42
#
# javak-lsp vs jdtls on real projects; only the answers that differ are shown.
benchmark repos="" seed="":
    cargo build --release
    JDTLS_COMPAT=1 JDTLS_COMPAT_REPOS="{{repos}}" JDTLS_COMPAT_SEED="{{seed}}" \
        cargo test --release --lib {{benchmark_test}} -- --nocapture --test-threads=1

# The same, asking jdtls again only when the project or the questions changed.
benchmark-quick repos="" seed="":
    cargo build --release
    JDTLS_COMPAT=1 JDTLS_COMPAT_REUSE=1 JDTLS_COMPAT_REPOS="{{repos}}" JDTLS_COMPAT_SEED="{{seed}}" \
        cargo test --release --lib {{benchmark_test}} -- --nocapture --test-threads=1
