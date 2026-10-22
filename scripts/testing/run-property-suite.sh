#!/usr/bin/env bash
#
# run-property-suite.sh — property/invariant/journey test runner with test
# performance benchmarking (issues #686, #693).
#
# Builds the lending integration suites once, then runs each suite binary on
# its own and records wall time, test counts and proptest cases to
# $OUT (default stellar-lend/target/test-performance.json). Durations are
# compared with the committed baseline; a suite slower than
# SLOWDOWN_FACTOR × baseline is reported (and fails the run when
# ENFORCE_PERF=1). Invariant violations are written by the suites to
# $INVARIANT_REPORT_DIR for CI alerting.
#
#   PROPTEST_CASES=256 scripts/testing/run-property-suite.sh
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SL="${ROOT}/stellar-lend"
SUITES=(${SUITES:-fuzz_invariants contract_invariants user_journeys event_topics})
OUT="${OUT:-${SL}/target/test-performance.json}"
BASELINE="${BASELINE:-${SL}/benchmarks/test-performance-baseline.json}"
SLOWDOWN_FACTOR="${SLOWDOWN_FACTOR:-2.0}"
export PROPTEST_CASES="${PROPTEST_CASES:-64}"
export INVARIANT_REPORT_DIR="${INVARIANT_REPORT_DIR:-${SL}/target/invariant-violations}"
export JOURNEY_REPORT_DIR="${JOURNEY_REPORT_DIR:-${SL}/target/journey-reports}"
rm -rf "${INVARIANT_REPORT_DIR}"
mkdir -p "$(dirname "${OUT}")"

log() { printf '[property-suite] %s\n' "$*" >&2; }

cd "${SL}"
test_args=()
for s in "${SUITES[@]}"; do test_args+=(--test "$s"); done

log "building ${SUITES[*]}"
cargo test -p stellarlend-lending "${test_args[@]}" --no-run 2>&1 | tail -n 3 >&2

status=0
results=()
for suite in "${SUITES[@]}"; do
    log "running ${suite} (PROPTEST_CASES=${PROPTEST_CASES})"
    start=$(date +%s.%N)
    set +e
    output=$(cargo test -p stellarlend-lending --test "${suite}" 2>&1)
    code=$?
    set -e
    end=$(date +%s.%N)
    echo "${output}" | grep -E '^test |test result|INVARIANT VIOLATION|panicked' >&2 || true
    passed=$(echo "${output}" | grep -oE '[0-9]+ passed' | tail -1 | grep -oE '[0-9]+' || echo 0)
    failed=$(echo "${output}" | grep -oE '[0-9]+ failed' | tail -1 | grep -oE '[0-9]+' || echo 0)
    [[ ${code} -ne 0 ]] && status=1
    results+=("$(printf '{"suite":"%s","seconds":%.3f,"passed":%s,"failed":%s,"exit_code":%s}' \
        "${suite}" "$(python3 -c "print(${end} - ${start})")" "${passed}" "${failed}" "${code}")")
done

python3 - "${OUT}" "${BASELINE}" "${SLOWDOWN_FACTOR}" "${PROPTEST_CASES}" "${results[@]}" <<'PY'
import json, os, sys
out, baseline_path, factor, cases = sys.argv[1], sys.argv[2], float(sys.argv[3]), int(sys.argv[4])
suites = [json.loads(s) for s in sys.argv[5:]]
baseline = {}
if os.path.exists(baseline_path):
    with open(baseline_path) as f:
        baseline = {s["suite"]: s for s in json.load(f).get("suites", [])}

slow = []
for s in suites:
    b = baseline.get(s["suite"])
    # Baselines are recorded at a known case count; scale property suites linearly.
    if b:
        expected = b["seconds"] * (cases / b.get("proptest_cases", cases) if s["suite"] == "fuzz_invariants" else 1)
        s["baseline_seconds"] = round(expected, 3)
        s["slowdown"] = round(s["seconds"] / expected, 2) if expected else None
        # Absolute floor so sub-second suites don't flag on runner jitter.
        if expected and s["seconds"] > expected * factor and s["seconds"] - expected > 5:
            slow.append(s["suite"])

report = {"proptest_cases": cases, "slowdown_factor": factor, "suites": suites, "slow_suites": slow}
with open(out, "w") as f:
    json.dump(report, f, indent=2)

summary = os.environ.get("GITHUB_STEP_SUMMARY")
lines = ["## Property / journey suite performance", "",
         f"PROPTEST_CASES={cases}", "",
         "| Suite | Passed | Failed | Seconds | Baseline | Slowdown |", "| --- | ---: | ---: | ---: | ---: | ---: |"]
for s in suites:
    lines.append(f"| {s['suite']} | {s['passed']} | {s['failed']} | {s['seconds']} | {s.get('baseline_seconds', '—')} | {s.get('slowdown', '—')}× |")
if slow:
    lines += ["", f"⚠️ Slower than {factor}× baseline: {', '.join(slow)}"]
text = "\n".join(lines) + "\n"
print(text, file=sys.stderr)
if summary:
    with open(summary, "a") as f:
        f.write(text)
for s in slow:
    print(f"::warning::{s} ran {factor}× slower than its baseline", file=sys.stderr)
PY

slow_count=$(python3 -c "import json; print(len(json.load(open('${OUT}'))['slow_suites']))")
if [[ "${ENFORCE_PERF:-0}" == "1" && "${slow_count}" != "0" ]]; then
    log "performance regression in ${slow_count} suite(s)"
    status=1
fi

if compgen -G "${INVARIANT_REPORT_DIR}/*.json" > /dev/null; then
    log "invariant violations recorded in ${INVARIANT_REPORT_DIR}"
    status=1
fi
exit ${status}
