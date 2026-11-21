# fixtures

Everything here is either a cut of a real `LUCINANET_MEDIA_TRACE` log or a hand-written log in exactly the shapes the backend
prints. The real ones are worth more: they are what the checker has to be right about. Nothing in them is edited apart from
being cut — no line was retyped, no value smoothed. URLs never appear: the excerpts carry only `lucinanet-media:` lines, which
name source files inside the repo and no site.

## real excerpts

| fixture | cut from | lines | what it is worth |
| --- | --- | --- | --- |
| `yt-f4-freeze.log` | `<work-dir>/lucinanet/nav05-fix/yt-diag-f4.log` | 129 | case F4 on YouTube, as it happened: one contiguous slice, `8.90–13.40 s`, the last two seconds before the freeze and the four after it. The page pauses at 9.115 s and the latch goes up (`playing true -> false held=Some(173734) from …mse_play.rs:395`); it resumes at 9.556 s and is paused again 2 ms later, and the pair repeats at 9.565 s from `:445` beside `audio cmd Paused false` and `Paused true` — `:395` and `:445` are that build's line numbers, and both flip the flag in both directions, so neither is `impl Drop for MseSound`: they are the page's own `set_paused` path and the sound thread applying `Cmd::Paused` from its queue. From there to the end of the run the position answers `3619458 µs` — the latched 173734 frames at 48 kHz — with `queued=96000` and 30 s buffered, and no `show` line at all, while the page's probe keeps reporting its own time (`t=9227` at 9.500, `t=11276` at 11.547). The checker has to name the clock, and does: clock-contract at t+11.578. Contiguous because a window with a hole in it looks like a clock that stopped moving. |
| `fix-play2-rec1-healthy.log` | `<work-dir>/lucinanet/nav05-fix/fix-play2-rec1.log` | 67 | The same local `play2` page with the fixed media crate: open, `player play`, latch released, plays, page-pauses at 2.340 s and stays paused. Windows `0.19–0.40`, `2.20–2.40`, `4.28–4.29`, `52.70–52.71` — the last two are heartbeats 50 s apart, which is only allowed because the stream is paused in between. Has to come back clean. |
| `nav05-120-rec1-resume.log` | `<work-dir>/lucinanet/yt-play/logs/nav05-fix-120-rec1.log` | 26 | A contiguous 0.5 s of YouTube with the fixed crate (`14.79–15.27`): page pause, resume with the latch released (`latch 383478 released`), a second pause 8 ms later, and the room wait that follows it. Contiguous on purpose — a window with a hole in it would look like a clock that stopped moving. Has to come back clean. |
| `yt2-rec0-new-player.log` | `<work-dir>/lucinanet/nav05-accept/yt2-rec0.log` | 38 | The page navigation that the checker called a clock running backwards, and the reason it must not: the old element ends its run at `now=Some(7348250)` with its `played VP9 seconds=9.4 …` summary, then the engine writes five `phase` lines — `load started`, `first frame painted`, `load complete`, a second `load started`, `first frame painted` — the new player's open fails (`no pictures:`, `no sound:`, `opened: no`), the mixer shows the fresh zeroed stream, and the first thing the new clock says is `show pts=0 now=Some(0)`. Two clocks, one after the other. The five `phase` lines stay as the file has them, and only the two `load started` ones are boundaries: the other three report the document that just ended, which is the distinction `src/lib.rs` fixes and a unit test pins. Contiguous apart from the fetch and probe noise that was dropped. Has to come back clean. |
| `yt-rec0-navigation.log` | `<work-dir>/lucinanet/crash10/yt-rec0.log` | 329 | a YouTube run that navigated to the consent page 34 s in, so the old document's last `event=play-redundant … potentially_playing=1` is followed, four thousand lines later, by the new document's first `event=play … player_playing=0`. Windows `520–700`, `2260–2360`, `6760–6800`, `7275–7310`, `7500–7650`, `9440–9470`, `9660–9685` — the four element lines of the old document, the two `load started` of the navigation with the old player's `played VP9 seconds=34.0` summary between them, and the new document's open and its own `play-redundant` pair. The bare `phase` lines stay because they are where the boundary is. Unfixed the checker named element-contract-1 at line 165 "seen at line 188"; it has to come back clean, and `a_navigation_ends_the_element_the_state_was_about` also reads it with the `phase` lines taken out, where the violation comes back. |
| `prefix-play2-M-frozen.log` | `<work-dir>/lucinanet/nav05-fix/prefix-play2-M.log` | 17 | A run of the old binary with `LUCINANET_MEDIA_TRACE` set and not one `lucinanet-media:` line in the output — the binary predates the trace. It is the shape of "the tool was pointed at a log that says nothing": no violations, no passes, eight properties reported as not observed. |

Cut with `sed` over the `[t+ …]` stamp and `grep` for the `lucinanet-media:` mark — except that
`yt2-rec0-new-player.log` and `yt-rec0-navigation.log` also keep the engine's bare `phase` lines, which carry no mark and are
where the file's boundary is (`yt-rec0-navigation.log` keeps only the `phase <mono> …` ones: `phase navigation Ok(…)` carries
the watch URL and is noise the checker does not read). Then checked for leftovers (`grep -cE "://|netphase|fetch#|gstatic|ytimg"` returns 0 on every
file here).

## synthetic

`healthy-all.log` is 17 lines in the shapes today's trace prints, arranged so that all eight properties get judged and none
breaks. It is the passing fixture for every property, and the base every planted file is cut from. The element lines use the
`media elem …` format `src/lib.rs` fixes for the fork; the heartbeat carries `in=` and `out=` beside `taken=`/`heard=`, and one
`audio wait frame` line parks on the cursor with `frames=0`, which is the honest version of that wait.

Each file in `planted/` is `healthy-all.log` with exactly one fact changed, and its name is the id that has to come back:

| file | the planted break |
| --- | --- |
| `element-contract-1.log` | line 1 says `potentially_playing=0` right before the player starts. |
| `element-contract-2.log` | line 1 finds the player playing and sends `cmd=play` anyway. |
| `element-contract-3.log` | line 1 sends `cmd=pause` with `paused=0 ended=0 seeking=0`. |
| `element-contract-4.log` | the play request never reaches the mixer: the `player play`, the resume, the `Paused false` and the mixer's own transition are gone, so the first line that says the player plays is the 2.1 s heartbeat. |
| `MEDIA-STR-held.log` | the `false -> true` transition keeps `held=Some(144048)`. |
| `MEDIA-STR-uncleared.log` | the resume and both transitions to `true` are gone, and the heartbeat says `playing=false held=Some(144048)`; the request at 0.1 s is never answered. |
| `MEDIA-STR-RING.log` | the heartbeat's `out=` no longer adds up with `queued=` to `in=`. |
| `clock-contract-backward.log` | the clock answers 900000 µs after it had reached 1101500 µs, with no seek between. |
| `clock-contract-stall.log` | the pause cluster is gone and the clock answers the same frame from 2.4 s to 5.4 s while audio sits buffered. |
| `MEDIA-CUR.log` | the frame wait says `frames=1500` with the cursor at 2 s and the buffer ending at 30 s. |
