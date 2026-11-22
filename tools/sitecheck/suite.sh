#!/usr/bin/env bash
# suite.sh: render every site in sites.txt twice, once with the LucinaNet
# binary and once with headless Firefox as the reference, then write
# OUT/report.md with the log table and the pixel diff of each pair.
#
# Sites run one at a time on purpose: both engines are heavy, and running them
# side by side would distort the timings the log table reports.

set -u -o pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)

# Set WORK_DIR to a scratch directory holding the engine binary and the run output (no default: a
# missing one stops here rather than writing into /tmp).
WORK_DIR=${WORK_DIR:?set WORK_DIR to a scratch directory (not /tmp)}

bin=$WORK_DIR/lucinanet/health/bin/lucinanet-servo-after2
out=$WORK_DIR/lucinanet/health/sitesuite
size=1280x800
only=
firefox=${FIREFOX:-firefox}
sites=${SITES:-$here/sites.txt}
threshold=${THRESHOLD:-24}
seconds=${TIMEOUT:-150}

usage() {
	cat <<'EOF'
usage: suite.sh [--bin PATH] [--out DIR] [--only name,name] [--size WxH]

  --bin PATH    lucinanet binary to render with
  --out DIR     where the pngs, logs and report.md go
  --only LIST   comma separated subset of the names in sites.txt
  --size WxH    viewport for both engines (default 1280x800)

Environment:
  SITECHECK   sitecheck binary; without it the script uses `cargo run --release`
  FIREFOX     reference browser command (default: firefox)
  SITES       site list (default: sites.txt next to this script)
  THRESHOLD   per-channel diff threshold handed to `sitecheck diff` (24)
  TIMEOUT     seconds allowed for one render of one site (150)
  WORK_DIR    scratch dir the default --bin and --out paths sit under: required
  TMPDIR      scratch dir for this run's temp dirs: required, nothing under /tmp
EOF
}

while [ $# -gt 0 ]; do
	case $1 in
	--bin | --out | --only | --size | --firefox | --sites | --threshold)
		[ $# -ge 2 ] || { printf 'suite.sh: %s needs a value\n' "$1" >&2; exit 2; }
		flag=$1
		value=$2
		shift 2
		case $flag in
			--bin) bin=$value ;;
			--out) out=$value ;;
			--only) only=$value ;;
			--size) size=$value ;;
			--firefox) firefox=$value ;;
			--sites) sites=$value ;;
			--threshold) threshold=$value ;;
		esac
		;;
	-h | --help)
		usage
		exit 0
		;;
	*)
		printf 'suite.sh: unknown argument: %s\n' "$1" >&2
		usage >&2
		exit 2
		;;
	esac
done

width=${size%x*}
height=${size#*x}
for dimension in "$width" "$height"; do
	case $dimension in '' | *[!0-9]*) printf 'suite.sh: bad --size %s, want WxH\n' "$size" >&2; exit 2 ;; esac
done
case $threshold in '' | *[!0-9]*) printf 'suite.sh: bad threshold %s\n' "$threshold" >&2; exit 2 ;; esac

[ -r "$sites" ] || { printf 'suite.sh: cannot read %s\n' "$sites" >&2; exit 2; }
[ -x "$bin" ] || { printf 'suite.sh: no lucinanet binary at %s\n' "$bin" >&2; exit 2; }
command -v "$firefox" >/dev/null 2>&1 || {
	printf 'suite.sh: no reference browser (%s); set FIREFOX\n' "$firefox" >&2
	exit 2
}

if [ -n "${SITECHECK:-}" ]; then
	[ -x "$SITECHECK" ] || { printf 'suite.sh: SITECHECK is not executable: %s\n' "$SITECHECK" >&2; exit 2; }
	sitecheck() { "$SITECHECK" "$@"; }
else
	sitecheck() { (cd -- "$here" && cargo run --release --quiet -- "$@"); }
fi

mkdir -p -- "$out" || exit 2
out=$(cd -- "$out" && pwd)
work=$(mktemp -d "${TMPDIR:?set TMPDIR to a scratch directory (not /tmp)}/sitecheck-suite.XXXXXX") || exit 2
trap 'rm -rf -- "$work"' EXIT

# A name that is not in the list means a typo in --only, and silently running
# nothing would be the worst way to find that out.
if [ -n "$only" ]; then
	IFS=, read -ra wanted <<<"$only"
	for name in "${wanted[@]}"; do
		awk -v want="$name" '$1 == want { found = 1 } END { exit !found }' "$sites" || {
			printf 'suite.sh: no site named %s in %s\n' "$name" "$sites" >&2
			exit 2
		}
	done
fi

selected=0
failed=0
: >"$work/ranked"

# The `# url` header is what names the site in the log table.
render_lucinanet() {
	local name=$1 url=$2
	printf '# url %s\n' "$url" >"$out/$name.lucinanet.err"
	LUCINANET_TIMING=1 LUCINANET_CONSOLE=1 LUCINANET_SCROLL_TEST=6 LUCINANET_PROFILE_DIR=none \
		timeout -s KILL "$seconds" "$bin" --render "$url" "$out/$name.lucinanet.png" "${width}x${height}" \
		2>>"$out/$name.lucinanet.err"
}

# A fresh profile each time, so no cache or session restore changes the reference.
render_firefox() {
	local name=$1 url=$2 profile rc
	profile=$(mktemp -d "${TMPDIR:?set TMPDIR to a scratch directory (not /tmp)}/sitecheck-ff.XXXXXX") || return 1
	timeout "$seconds" "$firefox" --headless --no-remote \
		--screenshot "$out/$name.firefox.png" --window-size="${width},${height}" \
		-profile "$profile" "$url" >"$out/$name.firefox.log" 2>&1
	rc=$?
	rm -rf -- "$profile"
	return $rc
}

png_state() {
	if [ -s "$1" ]; then printf 'png %s bytes' "$(wc -c <"$1" | tr -d ' ')"
	else printf 'no png'; fi
}

# Cut short so one runaway log line cannot fill the report.
log_tail() {
	[ -r "$1" ] && tail -n 5 -- "$1" | cut -c1-200
}

printf 'suite.sh: %s vs %s, %sx%s, threshold %s\n' "$bin" "$firefox" "$width" "$height" "$threshold"
printf 'suite.sh: output in %s\n' "$out"

while read -r name url _ || [ -n "${name:-}" ]; do
	case ${name:-} in
	'' | \#*) continue ;;
	esac
	if [ -n "$only" ]; then
		case ",$only," in *",$name,"*) ;; *) continue ;; esac
	fi
	selected=$((selected + 1))

	# A stale image from an earlier run would compare against the wrong thing.
	rm -f -- "$out/$name.lucinanet.png" "$out/$name.firefox.png"

	printf 'suite.sh: [%s] %s %s\n' "$selected" "$name" "$url"
	render_lucinanet "$name" "$url"
	rc_lucinanet=$?
	render_firefox "$name" "$url"
	rc_firefox=$?

	loaded=no
	grep -q '^render: complete true' "$out/$name.lucinanet.err" 2>/dev/null && loaded=yes

	# Compare whenever there are two images, even if the load failed: the
	# numbers are what tell how wrong a half-loaded page actually is.
	status=ok
	pct=
	if [ -s "$out/$name.lucinanet.png" ] && [ -s "$out/$name.firefox.png" ]; then
		sitecheck diff "$out/$name.lucinanet.png" "$out/$name.firefox.png" \
			--threshold "$threshold" >"$work/diff.$name" 2>&1 || true
		pct=$(sed -n 's/^diff: \([0-9][0-9.]*\)%.*$/\1/p' "$work/diff.$name")
	fi
	sort_key=${pct:-100.00}

	if [ ! -s "$out/$name.lucinanet.png" ]; then
		status="lucinanet produced no png (exit $rc_lucinanet)"
	elif [ ! -s "$out/$name.firefox.png" ]; then
		status="firefox produced no png (exit $rc_firefox)"
	elif [ "$loaded" != yes ]; then
		status="lucinanet did not report a completed load"
	elif [ -z "$pct" ]; then
		status="sitecheck diff gave no percentage"
	fi

	block=$work/block.$name
	if [ "$status" = ok ]; then
		{
			printf '### %s — %s%% of pixels differ\n\n' "$name" "$pct"
			printf '```\n%s\n```\n' "$(cat "$work/diff.$name")"
		} >"$block"
	else
		failed=$((failed + 1))
		sort_key=100.00 # no comparison is worse than any comparison
		{
			printf '### %s — FAILED\n\n' "$name"
			printf '%s\n\n' "$status"
			printf -- '- lucinanet: exit %s, loaded %s, %s\n' \
				"$rc_lucinanet" "$loaded" "$(png_state "$out/$name.lucinanet.png")"
			printf -- '- firefox: exit %s, %s\n' "$rc_firefox" "$(png_state "$out/$name.firefox.png")"
			if [ -s "$work/diff.$name" ]; then
				printf '\ndiff:\n\n```\n%s\n```\n' "$(cat "$work/diff.$name")"
			fi
			printf '\nlast lines of `%s.lucinanet.err`:\n\n```\n%s\n```\n' \
				"$name" "$(log_tail "$out/$name.lucinanet.err")"
			printf '\nlast lines of `%s.firefox.log`:\n\n```\n%s\n```\n' \
				"$name" "$(log_tail "$out/$name.firefox.log")"
		} >"$block"
		printf 'suite.sh: [%s] %s: %s\n' "$selected" "$name" "$status"
	fi
	printf '%s\t%s\n' "$sort_key" "$name" >>"$work/ranked"
done <"$sites"

report=$out/report.md
{
	printf '# Site suite\n\n'
	printf -- '- run: %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
	printf -- '- binary: `%s`\n' "$bin"
	printf -- '- reference: `%s --headless --screenshot --window-size=%s,%s`\n' "$firefox" "$width" "$height"
	printf -- '- viewport: %sx%s, threshold: %s per channel\n' "$width" "$height" "$threshold"
	printf -- '- sites: %s selected, %s failed to compare\n\n' "$selected" "$failed"

	printf '## LucinaNet log table\n\n'
	shopt -s nullglob
	logs=("$out"/*.lucinanet.err)
	shopt -u nullglob
	if [ ${#logs[@]} -eq 0 ]; then
		printf 'no .err logs in %s\n' "$out"
	else
		sitecheck "${logs[@]}" || printf 'sitecheck failed on the logs in %s\n' "$out"
	fi

	printf '\n## Pixel diff against Firefox, worst first\n\n'
	printf 'Firefox screenshots the viewport, so both sides are compared from\n'
	printf 'the top left over the area they share (%sx%s or less).\n\n' "$width" "$height"
	if [ "$selected" -eq 0 ]; then
		printf 'no site matched the selection\n'
	else
		while IFS=$'\t' read -r pct name; do
			cat "$work/block.$name"
			printf '\n'
		done < <(LC_ALL=C sort -t $'\t' -k1,1nr -k2,2 "$work/ranked")
	fi
} >"$report"

printf 'suite.sh: report in %s (%s of %s sites failed to compare)\n' "$report" "$failed" "$selected"
