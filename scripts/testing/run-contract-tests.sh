#!/usr/bin/env bash
#
# run-contract-tests.sh — single entry point for the unified contract test
# suite built on stellar-lend/contracts/test-utils (issue #700).
#
#   scripts/testing/run-contract-tests.sh [test|bench|coverage|list]
#
#   test      run every suite, record per-suite wall time and test counts
#             (default)
#   bench     run the gas benchmark suites and collect their JSON reports
#   coverage  run every suite under cargo-llvm-cov and write lcov + summary
#   list      print the suites and the cargo arguments used for each
#
# Environment:
#   SUITES           space-separated subset of suite names (default: all)
#   PROPTEST_CASES   cases per property in fuzz suites (default 16)
#   TEST_SEED        seed for seeded test data (default: test-utils DEFAULT_SEED)
#   REPORT_DIR       output directory (default stellar-lend/target/test-reports)
#   COVERAGE_MIN     fail `coverage` when line coverage is below this percent
#   COVERAGE_HTML=1  also write an HTML coverage report
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SL="${ROOT}/stellar-lend"
REPORT_DIR="${REPORT_DIR:-${SL}/target/test-reports}"
export PROPTEST_CASES="${PROPTEST_CASES:-16}"

# Suite registry: name -> cargo test arguments. Add new suites here; CI,
# coverage and the docs all read from this list.
declare -A SUITE_ARGS=(
    [test-utils]="-p test-utils"
    [common]="-p stellarlend-common --lib"
    [lending-integration]="-p stellarlend-lending --test admin_config_auth --test config_auth --test incident_regressions --test event_topics --test protocol_formal_specs --test scenarios --test user_journeys --test fuzz_invariants --test supplier_rewards --test supply_caps --test borrow_caps"
    [flash-loan-liquidation]="-p flash-loan-liquidation-tests"
)
SUITE_ORDER=(test-utils common lending-integration flash-loan-liquidation)
BENCH_ARGS="-p test-utils --test benchmarks"
JOURNEY_ARGS="-p stellarlend-lending --test user_journeys"

read -r -a SELECTED <<< "${SUITES:-${SUITE_ORDER[*]}}"
for s in "${SELECTED[@]}"; do
    [[ -n "${SUITE_ARGS[$s]:-}" ]] || { echo "unknown suite: $s (see: $0 list)" >&2; exit 2; }
done

log() { printf '[contract-tests] %s\n' "$*" >&2; }

summary() {
    if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
        cat >> "${GITHUB_STEP_SUMMARY}"
    else
        cat >&2
    fi
}

cmd_list() {
    for s in "${SUITE_ORDER[@]}"; do
        printf '%-24s cargo test %s\n' "$s" "${SUITE_ARGS[$s]}"
    done
}

cmd_test() {
    mkdir -p "${REPORT_DIR}"
    cd "${SL}"
    local status=0 results=()
    for suite in "${SELECTED[@]}"; do
        local log_file="${REPORT_DIR}/${suite}.log" start end code
        log "building ${suite}"
        # shellcheck disable=SC2086
        cargo test ${SUITE_ARGS[$suite]} --no-run 2>&1 | tail -n 1 >&2
        log "running ${suite}"
        start=$(date +%s.%N)
        set +e
        # shellcheck disable=SC2086
        cargo test ${SUITE_ARGS[$suite]} > "${log_file}" 2>&1
        code=$?
        set -e
        end=$(date +%s.%N)
        grep -E '^test result|FAILED|panicked' "${log_file}" >&2 || true
        [[ ${code} -ne 0 ]] && status=1
        results+=("$(python3 - "${suite}" "${start}" "${end}" "${code}" "${log_file}" <<'PY'
import json, re, sys
suite, start, end, code, log_file = sys.argv[1], float(sys.argv[2]), float(sys.argv[3]), int(sys.argv[4]), sys.argv[5]
with open(log_file) as f:
    text = f.read()
totals = {"passed": 0, "failed": 0, "ignored": 0}
for m in re.finditer(r"test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored", text):
    for key, value in zip(totals, m.groups()):
        totals[key] += int(value)
print(json.dumps({"suite": suite, "seconds": round(end - start, 3), "exit_code": code, **totals}))
PY
)")
    done

    python3 - "${REPORT_DIR}/contract-tests.json" "${PROPTEST_CASES}" "${results[@]}" <<'PY' | summary
import json, os, sys
out, cases = sys.argv[1], int(sys.argv[2])
suites = [json.loads(s) for s in sys.argv[3:]]
report = {
    "proptest_cases": cases,
    "test_seed": os.environ.get("TEST_SEED", "default"),
    "totals": {k: sum(s[k] for s in suites) for k in ("passed", "failed", "ignored")},
    "suites": suites,
}
with open(out, "w") as f:
    json.dump(report, f, indent=2)
print("## Unified contract test suite\n")
print(f"PROPTEST_CASES={cases} · TEST_SEED={report['test_seed']}\n")
print("| Suite | Passed | Failed | Ignored | Seconds |")
print("| --- | ---: | ---: | ---: | ---: |")
for s in suites:
    mark = "" if s["exit_code"] == 0 else " ❌"
    print(f"| {s['suite']}{mark} | {s['passed']} | {s['failed']} | {s['ignored']} | {s['seconds']} |")
t = report["totals"]
print(f"| **total** | {t['passed']} | {t['failed']} | {t['ignored']} | {round(sum(s['seconds'] for s in suites), 3)} |")
PY
    log "report: ${REPORT_DIR}/contract-tests.json"
    return ${status}
}

cmd_bench() {
    local dir="${REPORT_DIR}/benchmarks"
    mkdir -p "${dir}"
    cd "${SL}"
    # shellcheck disable=SC2086
    BENCH_REPORT_DIR="${dir}" cargo test ${BENCH_ARGS} 2>&1 | grep -E '^test |test result' >&2
    # shellcheck disable=SC2086
    JOURNEY_REPORT_DIR="${dir}" cargo test ${JOURNEY_ARGS} 2>&1 | grep -E '^test |test result' >&2

    python3 - "${dir}" <<'PY' | summary
import json, os, sys
d = sys.argv[1]
print("## Gas benchmarks (CPU instructions per step)\n")
print("| Report | Journey | Step | CPU | Budget | Memory bytes |")
print("| --- | --- | --- | ---: | ---: | ---: |")
recs = []
for name in sorted(os.listdir(d)):
    if not name.endswith(".json"):
        continue
    with open(os.path.join(d, name)) as f:
        report = json.load(f)
    for j in report["journeys"]:
        for s in j["steps"]:
            flag = " ⚠️" if s["over_budget"] else ""
            print(f"| {name} | {j['name']} | {s['step']} | {s['cpu_instructions']}{flag} | {s['budget'] or '—'} | {s['memory_bytes']} |")
    recs += report.get("recommendations", [])
if recs:
    print("\n### Recommendations\n")
    for r in sorted(set(recs)):
        print(f"- {r}")
PY
    log "reports: ${dir}"
}

cmd_coverage() {
    command -v cargo-llvm-cov > /dev/null || {
        echo "cargo-llvm-cov is required: cargo install cargo-llvm-cov" >&2
        exit 2
    }
    local dir="${REPORT_DIR}/coverage"
    mkdir -p "${dir}"
    cd "${SL}"
    cargo llvm-cov clean --workspace
    for suite in "${SELECTED[@]}"; do
        log "coverage: ${suite}"
        # shellcheck disable=SC2086
        cargo llvm-cov --no-report ${SUITE_ARGS[$suite]} 2>&1 | grep -E '^test result|FAILED|panicked' >&2
    done
    # Only first-party contract and test code counts towards the numbers.
    local ignore='(/\.cargo/|/rustc/|/target/)'
    cargo llvm-cov report --ignore-filename-regex "${ignore}" --lcov --output-path "${dir}/lcov.info"
    cargo llvm-cov report --ignore-filename-regex "${ignore}" --summary-only > "${dir}/summary.txt"
    if [[ "${COVERAGE_HTML:-0}" == "1" ]]; then
        cargo llvm-cov report --ignore-filename-regex "${ignore}" --html --output-dir "${dir}"
    fi
    {
        echo "## Contract test coverage"
        echo
        echo '```'
        grep -E "test-utils/src|common/src|lending/src/(deposit|withdraw|borrow|views|pause)\.rs|^TOTAL" "${dir}/summary.txt"
        echo '```'
    } | summary
    if [[ -n "${COVERAGE_MIN:-}" ]]; then
        cargo llvm-cov report --ignore-filename-regex "${ignore}" --summary-only \
            --fail-under-lines "${COVERAGE_MIN}" > /dev/null
    fi
    log "coverage: ${dir}/lcov.info, ${dir}/summary.txt"
}

case "${1:-test}" in
    test) cmd_test ;;
    bench) cmd_bench ;;
    coverage) cmd_coverage ;;
    list) cmd_list ;;
    *) echo "usage: $0 [test|bench|coverage|list]" >&2; exit 2 ;;
esac
