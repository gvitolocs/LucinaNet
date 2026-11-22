#!/usr/bin/env bash
# ab.sh: run one LucinaNet binary over the sites with the two settings of ONE
# environment variable (the "arms"), interleaved round by round, and write
# OUT/ab.md with the median cold and same-process-warm time of each arm.
#
# Renders run strictly one at a time, and which arm goes first flips every
# round: machine drift then falls on both arms alike instead of on whichever
# setting happened to be measured in the fast half of the run.

set -u -o pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)

bin=
spec=
sites=$here/sites.txt
only=
rounds=5
out= # --out DIR, else $WORK_DIR/lucinanet/health/ab (resolved where it is used)
size=1280x800
cold_seconds=90  # one load in a new process
warm_seconds=150 # two loads in one process

usage() {
	cat <<'EOF'
usage: ab.sh --bin PATH --env 'NAME=A,NAME=B' [--sites FILE] [--only name,name]
             [--rounds N] [--out DIR] [--size WxH]

  --bin PATH    lucinanet binary to render with (required, must be executable)
  --env SPEC    the one variable and its two settings: NAME=A,NAME=B (the arms)
  --sites FILE  `name url` lines, # comments (default: sites.txt beside this script)
  --only LIST   comma separated subset of the names in the site list
  --rounds N    passes over the whole list, arm order flipped each pass (5)
  --out DIR     where ab.md, the per-run stderr and the profiles go (without it,
                under $WORK_DIR, else $TMPDIR: one of the two is then required)
  --size WxH    viewport (default 1280x800)

Per site and arm, every round, in this order:

  cold  a new process with a fresh empty profile dir, the URL loaded once:
        LUCINANET_TIMING=1 LUCINANET_RENDER_QUIET_MS=0 ... (timeout 90 s)
        the number is that run's `render: complete true in N ms`
  warm  ONE new process whose LOADS file lists the URL twice, timed on the
        second navigation (timeout 150 s):
        LUCINANET_LOADS=FILE LUCINANET_TIMING=1 ...
        the number is complete_ms of that run's `timing step=1` line

Each run keeps its stderr at OUT/<site>.<arm>.<round>.{cold,warm}.err and its
profile under OUT/profiles, which is removed as soon as the run is over.
Exit 2 is a bad argument or a binary that is not executable; a site that
fails is counted in the report and the run still exits 0.
EOF
}

while [ $# -gt 0 ]; do
	case $1 in
	--bin | --env | --sites | --only | --rounds | --out | --size)
		[ $# -ge 2 ] || { printf 'ab.sh: %s needs a value\n' "$1" >&2; exit 2; }
		flag=$1
		value=$2
		shift 2
		case $flag in
			--bin) bin=$value ;;
			--env) spec=$value ;;
			--sites) sites=$value ;;
			--only) only=$value ;;
			--rounds) rounds=$value ;;
			--out) out=$value ;;
			--size) size=$value ;;
		esac
		;;
	-h | --help)
		usage
		exit 0
		;;
	*)
		printf 'ab.sh: unknown argument: %s\n' "$1" >&2
		usage >&2
		exit 2
		;;
	esac
done

[ -n "$bin" ] || { printf 'ab.sh: --bin is required\n' >&2; usage >&2; exit 2; }
[ -n "$spec" ] || { printf 'ab.sh: --env is required\n' >&2; usage >&2; exit 2; }
[ -x "$bin" ] || { printf 'ab.sh: not an executable binary: %s\n' "$bin" >&2; exit 2; }
[ -r "$sites" ] || { printf 'ab.sh: cannot read %s\n' "$sites" >&2; exit 2; }

width=${size%x*}
height=${size#*x}
case $size in
*[0-9]x[0-9]*) ;;
*) printf 'ab.sh: bad --size %s, want WxH\n' "$size" >&2; exit 2 ;;
esac
for dimension in "$width" "$height"; do
	case $dimension in '' | *[!0-9]*) printf 'ab.sh: bad --size %s, want WxH\n' "$size" >&2; exit 2 ;; esac
done
case $rounds in '' | 0 | *[!0-9]*) printf 'ab.sh: bad --rounds %s, want 1 or more\n' "$rounds" >&2; exit 2 ;; esac

# One variable, two settings. Two different names would measure two things at
# once, and a missing or odd name is a typo in the spec rather than an arm.
IFS=, read -ra parts <<<"$spec"
[ ${#parts[@]} -eq 2 ] || { printf "ab.sh: --env wants NAME=A,NAME=B, got '%s'\n" "$spec" >&2; exit 2; }
for part in "${parts[@]}"; do
	case $part in
	*=*) ;;
	*) printf "ab.sh: --env part with no '=': '%s'\n" "$part" >&2; exit 2 ;;
	esac
	case ${part%%=*} in
	'' | [0-9]* | *[!A-Za-z0-9_]*) printf "ab.sh: not a variable name: '%s'\n" "${part%%=*}" >&2; exit 2 ;;
	esac
done
var_name=${parts[0]%%=*}
[ "$var_name" = "${parts[1]%%=*}" ] || {
	printf 'ab.sh: --env names differ: %s and %s\n' "$var_name" "${parts[1]%%=*}" >&2
	exit 2
}
arm_a="$var_name=${parts[0]#*=}"
arm_b="$var_name=${parts[1]#*=}"

names=()
urls=()
while read -r name url _ || [ -n "${name:-}" ]; do
	case ${name:-} in
	'' | \#*) continue ;;
	esac
	if [ -z "${url:-}" ]; then
		printf 'ab.sh: %s: site %s has no url\n' "$sites" "$name" >&2
		exit 2
	fi
	names+=("$name")
	urls+=("$url")
done <"$sites"
all_csv=$(IFS=,; printf '%s' "${names[*]}")

# A name that is not in the list is a typo in --only, and silently measuring
# nothing would be the worst way to find that out.
if [ -n "$only" ]; then
	IFS=, read -ra wanted <<<"$only"
	for name in "${wanted[@]}"; do
		case ",$all_csv," in
		*",$name,"*) ;;
		*)
			printf 'ab.sh: no site named %s in %s\n' "$name" "$sites" >&2
			exit 2
			;;
		esac
	done
fi

sel_names=()
sel_urls=()
for i in "${!names[@]}"; do
	if [ -n "$only" ]; then
		case ",$only," in *,"${names[$i]},"*) ;; *) continue ;; esac
	fi
	sel_names+=("${names[$i]}")
	sel_urls+=("${urls[$i]}")
done
[ ${#sel_names[@]} -gt 0 ] || { printf 'ab.sh: no site selected\n' >&2; exit 2; }
sel_csv=$(IFS=,; printf '%s' "${sel_names[*]}")

[ -n "$out" ] || out=${WORK_DIR:-${TMPDIR:?set TMPDIR to a scratch directory (not /tmp)}}/lucinanet/health/ab
mkdir -p -- "$out/profiles" || exit 2
out=$(cd -- "$out" && pwd)
work=$(mktemp -d "${TMPDIR:?set TMPDIR to a scratch directory (not /tmp)}/sitecheck-ab.XXXXXX") || exit 2
trap 'rm -rf -- "$work"' EXIT
png=$work/render.png # one path, overwritten: only the timings are read back
: >"$work/ok.tsv"    # kind  site  arm  ms
: >"$work/bad.tsv"   # kind  site  arm  round  why

# The cold number: `render: complete true in N ms`. A build that prints the
# LOADS timing lines has the same moment as its step 0, read when there is no
# summary line to take it from.
cold_number() {
	local ms
	ms=$(sed -n 's/^render: complete true in \([0-9][0-9]*\) ms.*$/\1/p' "$1" | tail -n 1)
	if [ -z "$ms" ]; then
		ms=$(grep -m1 '^timing step=0 ' "$1" | sed -n 's/.* complete_ms=\([0-9][0-9.]*\).*$/\1/p')
	fi
	printf '%s' "$ms"
}

# The warm number: complete_ms of `timing step=1`, the second navigation of the
# one process. A `-` there means it never completed, so it is not a number.
warm_number() {
	sed -n 's/^timing step=1 .* complete_ms=\([0-9][0-9.]*\).*$/\1/p' "$1" | head -n 1
}

failure_why() { # kind rc err seconds
	local kind=$1 rc=$2 err=$3 seconds=$4 what
	if [ "$kind" = cold ]; then what='no completed cold load'; else what='no step=1 warm number'; fi
	case $rc in
	124 | 137 | 143)
		printf 'killed at the %s s timeout, %s' "$seconds" "$what"
		;;
	*) printf '%s (exit %s)' "$what" "$rc" ;;
	esac
	if [ -s "$err" ] && grep -q '^render: complete false' "$err"; then
		printf ', the load never finished'
	fi
	return 0
}

note_failure() { # kind site arm round why
	printf '%s\t%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4" "$5" >>"$work/bad.tsv"
	failed_runs=$((failed_runs + 1))
	printf 'ab.sh: r%s %s %s %s: FAILED %s\n' "$4" "$2" "$3" "$1" "$5"
}

run_measure() { # kind site url arm round
	local kind=$1 name=$2 url=$3 arm=$4 round=$5
	local armenv prof err rc ms seconds loads
	if [ "$arm" = a ]; then armenv=$arm_a; else armenv=$arm_b; fi
	if [ "$kind" = cold ]; then seconds=$cold_seconds; else seconds=$warm_seconds; fi
	err=$out/$name.$arm.$round.$kind.err
	if ! prof=$(mktemp -d -- "$out/profiles/$name.$arm.$round.$kind.XXXXXX"); then
		note_failure "$kind" "$name" "$arm" "$round" 'no profile dir could be made'
		return 1
	fi
	renders=$((renders + 1))
	if [ "$kind" = cold ]; then
		env LUCINANET_TIMING=1 LUCINANET_RENDER_QUIET_MS=0 LUCINANET_PROFILE_DIR="$prof" "$armenv" \
			timeout -s KILL "$seconds" "$bin" --render "$url" "$png" "${width}x${height}" 2>"$err"
		rc=$?
		ms=$(cold_number "$err")
	else
		# `timing` first, then the URL twice: step 0 is the load the command
		# line asked for, step 1 the navigation measured here.
		loads=$work/loads.$name
		printf 'timing\n%s\n%s\n' "$url" "$url" >"$loads"
		env LUCINANET_TIMING=1 LUCINANET_LOADS="$loads" LUCINANET_PROFILE_DIR="$prof" "$armenv" \
			timeout -s KILL "$seconds" "$bin" --render "$url" "$png" "${width}x${height}" 2>"$err"
		rc=$?
		ms=$(warm_number "$err")
	fi
	rm -rf -- "$prof" # a fresh dir per run, and never left lying on the disk
	if [ -n "$ms" ]; then
		printf '%s\t%s\t%s\t%s\n' "$kind" "$name" "$arm" "$ms" >>"$work/ok.tsv"
		measured=$((measured + 1))
		printf 'ab.sh: r%s %s %s %s %s ms\n' "$round" "$name" "$arm" "$kind" "$ms"
		return 0
	fi
	note_failure "$kind" "$name" "$arm" "$round" "$(failure_why "$kind" "$rc" "$err" "$seconds")"
	return 1
}

cat >"$work/table.awk" <<'AWK'
# One markdown table per measurement kind, over ok.tsv (kind site arm ms):
# per site the median and the min-max of each arm in ms, then b - a in ms and
# in percent, and last the median of the per-site deltas. The median of an
# even number of rounds is the mean of the two middle ones.
function sort_asc(a, n,   i, j, t) {
	for (i = 2; i <= n; i++) {
		t = a[i] + 0
		for (j = i - 1; j >= 1 && a[j] + 0 > t; j--) a[j + 1] = a[j]
		a[j + 1] = t
	}
}
function median(a, n,   m) {
	m = int((n + 1) / 2)
	if (n % 2) return a[m]
	return (a[m] + a[m + 1]) / 2
}
function stats(key,   P, n) {
	n = split(vals[key], P, " ")
	if (n == 0) { CNT = 0; LO = 0; HI = 0; MEDI = 0; return }
	sort_asc(P, n)
	CNT = n; LO = P[1]; HI = P[n]; MEDI = median(P, n)
}
function med_cell(m, c) {
	if (c == 0) return "n/a"
	if (c < rounds) return sprintf("%.1f (%d)", m, c)
	return sprintf("%.1f", m)
}
function range_cell(lo, hi, c) { return c == 0 ? "n/a" : sprintf("%.0f-%.0f", lo, hi) }
BEGIN {
	FS = "\t"
	n_sites = split(sites, S, ",")
	printf "| site | a median | a min-max | b median | b min-max | b-a ms | b-a pct |\n"
	printf "|---|---:|---:|---:|---:|---:|---:|\n"
}
$1 == kind { key = $2 SUBSEP $3; vals[key] = vals[key] (vals[key] == "" ? "" : " ") $4 }
END {
	nd = 0
	np = 0
	for (i = 1; i <= n_sites; i++) {
		site = S[i]
		stats(site SUBSEP "a"); ac = CNT; am = MEDI; alo = LO; ahi = HI
		stats(site SUBSEP "b"); bc = CNT; bm = MEDI; blo = LO; bhi = HI
		d = "n/a"
		p = "n/a"
		if (ac > 0 && bc > 0) {
			diff = bm - am
			d = sprintf("%+.1f", diff)
			nd++
			Delta[nd] = diff
			if (am > 0) {
				pct = diff / am * 100
				p = sprintf("%+.1f%%", pct)
				np++
				Pct[np] = pct
			}
		}
		printf "| %s | %s | %s | %s | %s | %s | %s |\n", \
			site, med_cell(am, ac), range_cell(alo, ahi, ac), med_cell(bm, bc), range_cell(blo, bhi, bc), d, p
	}
	printf "| **all sites, median of the deltas** | | | | |"
	if (nd == 0) {
		printf " n/a | n/a |\n"
		exit
	}
	sort_asc(Delta, nd)
	printf " %+.1f |", median(Delta, nd)
	if (np > 0) {
		sort_asc(Pct, np)
		printf " %+.1f%% |\n", median(Pct, np)
	} else {
		printf " n/a |\n"
	}
}
AWK

table() { # kind -> the markdown table of one measurement
	awk -F '\t' -v kind="$1" -v sites="$sel_csv" -v rounds="$rounds" -f "$work/table.awk" "$work/ok.tsv"
}

load1=$(awk '{print $1}' /proc/loadavg)
started=$(date -u '+%Y-%m-%dT%H:%M:%SZ')
renders=0
measured=0
failed_runs=0

printf 'ab.sh: %s\n' "$bin"
printf 'ab.sh: arms a = %s, b = %s\n' "$arm_a" "$arm_b"
printf 'ab.sh: %s sites x %s rounds x 2 arms x 2 measurements = %s renders, one at a time, load1 %s\n' \
	"${#sel_names[@]}" "$rounds" "$((${#sel_names[@]} * rounds * 4))" "$load1"

for round in $(seq 1 "$rounds"); do
	# The arm that starts each round swaps, so both arms get the same share of
	# whatever the machine is doing at the start of a round.
	if [ $((round % 2)) -eq 1 ]; then arm_order='a b'; else arm_order='b a'; fi
	i=0
	for name in "${sel_names[@]}"; do
		url=${sel_urls[$i]}
		i=$((i + 1))
		for arm in $arm_order; do
			run_measure cold "$name" "$url" "$arm" "$round"
			run_measure warm "$name" "$url" "$arm" "$round"
		done
	done
done

report=$out/ab.md
{
	printf '# A/B: `%s` against `%s`\n\n' "$arm_a" "$arm_b"

	printf '## cold - new process, empty profile, one load, ms\n\n'
	printf 'Arm `a` is `%s`, arm `b` is `%s`.\n\n' "$arm_a" "$arm_b"
	table cold

	printf '\n## same-process warm - the second navigation of one process, ms\n\n'
	printf 'One process loads the URL twice; the number is `timing step=1`, the\n'
	printf 'second navigation with the first one'"'"'s connection and caches still there.\n\n'
	table warm

	printf '\n## Settings\n\n'
	printf -- '- run: %s, load1 at start: %s\n' "$started" "$load1"
	printf -- '- binary: `%s` (mtime %s)\n' "$bin" "$(date -u -r "$bin" '+%Y-%m-%dT%H:%M:%SZ')"
	printf -- '- arms: `a` = `%s`, `b` = `%s` - one variable, so every delta is that switch\n' "$arm_a" "$arm_b"
	printf -- '- sites: %s of %s in `%s` (%s)\n' "${#sel_names[@]}" "${#names[@]}" "$sites" "$sel_csv"
	printf -- '- rounds: %s; the arm that goes first flips every round, and renders are never two at a time\n' "$rounds"
	printf -- '- viewport: %sx%s; timeouts: cold %s s, warm %s s\n' "$width" "$height" "$cold_seconds" "$warm_seconds"
	printf -- '- cold number: `render: complete true in N ms`; warm number: `complete_ms` of `timing step=1`\n'
	printf -- '- renders: %s measured, %s failed or timed out\n' "$measured" "$failed_runs"
	printf -- '- stderr of every run: `<site>.<arm>.<round>.<kind>.err` next to this file\n'
	printf -- '- comparable with another run of this script only on this machine at a similar load\n'

	printf '\n## Failed or timed-out runs\n\n'
	if [ -s "$work/bad.tsv" ]; then
		printf 'Each of these is left out of its arm'"'"'s median; the counts are in Settings.\n\n'
		printf '| measurement | site | arm | round | why |\n|---|---|---|---|---|\n'
		awk -F '\t' '{ printf "| %s | %s | %s | %s | %s |\n", $1, $2, $3, $4, $5 }' "$work/bad.tsv"
	else
		printf 'none\n'
	fi
} >"$report"

printf '\n'
table cold
printf '\n'
table warm
printf '\nab.sh: %s renders measured, %s failed or timed out; report in %s\n' "$measured" "$failed_runs" "$report"
exit 0
