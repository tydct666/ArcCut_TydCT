"""Verify native transition exports against source pixels and real source PTS.

Run only after native exports finish. Defaults:
  python tests/pause-transition-output-check.py

The source reference is selected by the export's actual keep plan. Both movies
are fully decoded with automatic display rotation and yuv420p pixel format.
No frames are generated from nominal FPS. Frozen protected frames are checked
at their source-to-output mapped positions, in addition to whole-output hashes.
"""
from __future__ import annotations

import argparse
import bisect
from fractions import Fraction
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_RUN = ROOT / "outputs/pause-transition-validation/final-run"
ANNOTATIONS = ROOT / "outputs/pause-transition-validation/annotations/annotations.json"
# Fixed acceptance limits, declared before running the new exports. Pixel
# hashes have zero tolerance. Microsecond rescaling permits at most2us.
PTS_TOLERANCE_US = 2
VIDEO_DURATION_TOLERANCE_US = 2000
AUDIO_TAIL_TOLERANCE_US = 40000  # Matches the existing native quality suite.


def read(path):
    return json.loads(Path(path).read_text(encoding="utf-8-sig"))


def save(path, data):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, ensure_ascii=False, indent=2), encoding="utf-8")


def sha_bytes(data):
    return hashlib.sha256(data).hexdigest()


def json_sha(value):
    return sha_bytes(json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8"))


def microseconds(text):
    if text is None or text == "N/A":
        return None
    return round(Fraction(str(text)) * 1000000)


def invoke(command, folder, name, timeout):
    stdout_path, stderr_path = folder / f"{name}.stdout", folder / f"{name}.stderr.txt"
    save(folder / f"{name}.command.json", list(map(str, command)))
    began = time.monotonic()
    proc = None
    try:
        with stdout_path.open("wb") as out, stderr_path.open("wb") as err:
            proc = subprocess.Popen(list(map(str, command)), stdout=out, stderr=err,
                                    creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
            while True:
                try:
                    proc.wait(timeout=20)
                    break
                except subprocess.TimeoutExpired:
                    elapsed = time.monotonic() - began
                    print(f"{name}: {elapsed:.0f}s elapsed", flush=True)
                    if elapsed > timeout:
                        raise TimeoutError(f"{name} exceeded {timeout}s")
        elapsed = round(time.monotonic() - began, 3)
        save(folder / f"{name}.execution.json", {"seconds": elapsed, "exitCode": proc.returncode})
        if proc.returncode:
            raise RuntimeError(f"{name}: {stderr_path.read_text(encoding='utf-8', errors='replace')[-5000:]}")
        return stdout_path, elapsed
    finally:
        if proc is not None and proc.poll() is None:
            proc.kill()
            proc.wait(timeout=20)


def probe(args, movie, folder, name, extra):
    path, _ = invoke([args.ffprobe, "-v", "error", *extra, "-of", "json", movie],
                     folder, name, args.timeout)
    return read(path)


def read_md5(path):
    time_bases, dimensions, rows = {}, {}, []
    for line in Path(path).read_text(encoding="utf-8").splitlines():
        match = re.match(r"#tb\s+(\d+):\s*(\d+)/(\d+)", line)
        if match:
            time_bases[int(match[1])] = Fraction(int(match[2]), int(match[3]))
            continue
        match = re.match(r"#dimensions\s+(\d+):\s*(\d+)x(\d+)", line)
        if match:
            dimensions[int(match[1])] = [int(match[2]), int(match[3])]
            continue
        if not line or line.startswith("#"):
            continue
        cells = [part.strip() for part in line.split(",")]
        stream = int(cells[0])
        if stream != 0:
            raise AssertionError("Unexpected non-video stream in video-only framemd5")
        if stream not in time_bases:
            raise AssertionError("framemd5 time base missing")
        pts = round(int(cells[2]) * time_bases[stream] * 1000000)
        rows.append({"ptsUs": pts, "bytes": int(cells[4]), "md5": cells[5]})
    return rows, dimensions.get(0)


def decode_md5(args, movie, folder, label, video_index, origin_us=0, keep=None):
    output = folder / f"{label}.framemd5"
    filters = ["settb=expr=1/1000000"]
    if origin_us:
        filters.append(f"setpts=PTS-({origin_us})")
    if keep is not None:
        expression = "+".join(f"gte(pts,{s['startUs']})*lt(pts,{s['endUs']})" for s in keep) or "0"
        filters.append(f"select='{expression}'")
    filters.append("format=yuv420p")
    command = [args.ffmpeg, "-hide_banner", "-v", "error", "-xerror", "-y", "-copyts", "-threads", "2",
               "-i", movie, "-map", f"0:{video_index}", "-an", "-sn", "-dn", "-filter_threads", "1",
               "-vf", ",".join(filters), "-c:v", "rawvideo", "-pix_fmt", "yuv420p",
               "-fps_mode", "passthrough", "-enc_time_base:v", "1:1000000", "-threads", "1",
               "-f", "framemd5", output]
    _, seconds = invoke(command, folder, label, args.timeout)
    rows, dimensions = read_md5(output)
    return rows, dimensions, seconds, output


def keep_mapping(project, plan):
    pts = project["source"]["framePts"]
    expected = []
    cursor = 0
    for span in plan["keep"]:
        a, b, output = span["startUs"], span["endUs"], span["outputUs"]
        if output != cursor or b <= a:
            raise AssertionError("Invalid or noncontiguous output keep plan")
        lo, hi = bisect.bisect_left(pts, a), bisect.bisect_left(pts, b)
        for t in pts[lo:hi]:
            expected.append((t, output + t - a))
        cursor += b - a
    if cursor != plan["durationUs"]:
        raise AssertionError("Keep duration does not equal actual export plan duration")
    return expected


def verify_one(args, kind, annotations):
    folder = args.run_dir / kind
    results_dir = args.run_dir / "output-check" / kind
    results_dir.mkdir(parents=True, exist_ok=True)
    state = read(folder / "export-result.json")
    if "job" in state and state["job"].get("status") != "completed":
        raise AssertionError(f"{kind}: native export was not completed")
    result = state.get("job", {}).get("result", state)
    plan = result["plan"]
    project = result.get("effectiveProject") or read(folder / "analyzed.arkcut.json")
    movie = folder / "processed.mp4"
    source = Path(project["source"]["fingerprint"]["path"])
    if not movie.is_file() or not movie.stat().st_size:
        raise FileNotFoundError(f"Missing native output: {movie}")
    movie_stat = movie.stat()
    source_stat = source.stat()
    expected = keep_mapping(project, plan)
    expected_source = [t for t, _ in expected]
    expected_output = [t for _, t in expected]
    failures = []
    report = {"video": kind, "status": "running", "path": str(movie), "source": str(source),
              "sourceFingerprint": project["source"]["fingerprint"], "planSha256": json_sha(plan),
              "usedEffectiveProject": bool(result.get("effectiveProject")),
              "pixelFormat": "yuv420p", "automaticDisplayRotation": True,
              "expectedFrames": len(expected), "expectedDurationUs": plan["durationUs"],
              "sourceAndOutputWereFullyDecoded": False, "failures": failures}
    source_rows, source_dimensions, source_seconds, source_hashes = decode_md5(
        args, source, results_dir, "source-kept", project["videoIndex"],
        project["source"].get("originUs", 0), plan["keep"])
    # Output video is selected from its metadata rather than assuming audio/video ordering.
    metadata = probe(args, movie, results_dir, "output-metadata", ["-show_streams", "-show_format"])
    video_streams = [s for s in metadata["streams"] if s["codec_type"] == "video"]
    audio_streams = [s for s in metadata["streams"] if s["codec_type"] == "audio"]
    if len(video_streams) != 1:
        raise AssertionError(f"Expected one exported video stream, found{len(video_streams)}")
    video = video_streams[0]
    output_rows, output_dimensions, output_seconds, output_hashes = decode_md5(
        args, movie, results_dir, "output", video["index"])
    report["sourceAndOutputWereFullyDecoded"] = True
    report["decodeSeconds"] = {"source": source_seconds, "output": output_seconds}
    report["hashFiles"] = {"source": str(source_hashes), "output": str(output_hashes)}
    report["decodedFrames"] = {"sourceKept": len(source_rows), "output": len(output_rows)}
    report["displayDimensions"] = {"source": source_dimensions, "output": output_dimensions}
    if len(source_rows) != len(expected) or len(output_rows) != len(expected):
        failures.append("Decoded source/output frame count does not equal actual source-PTS keep count")
    if source_dimensions != output_dimensions:
        failures.append("Decoded display dimensions differ (including automatic rotation)")
    source_errors = [abs(row["ptsUs"] - t) for row, t in zip(source_rows, expected_source)]
    output_errors = [abs(row["ptsUs"] - t) for row, t in zip(output_rows, expected_output)]
    source_monotonic = all(a["ptsUs"] < b["ptsUs"] for a, b in zip(source_rows, source_rows[1:]))
    output_monotonic = all(a["ptsUs"] < b["ptsUs"] for a, b in zip(output_rows, output_rows[1:]))
    report["pts"] = {"sourceMonotonic": source_monotonic, "outputMonotonic": output_monotonic,
                     "sourceMaxIndexErrorUs": max(source_errors, default=None),
                     "outputMaxMappingErrorUs": max(output_errors, default=None),
                     "allowedRescaleErrorUs": PTS_TOLERANCE_US,
                     "firstOutputUs": output_rows[0]["ptsUs"] if output_rows else None,
                     "lastOutputUs": output_rows[-1]["ptsUs"] if output_rows else None}
    if not source_monotonic or not output_monotonic:
        failures.append("Decoded presentation timestamps are not strictly increasing")
    if not source_errors or max(source_errors) > PTS_TOLERANCE_US:
        failures.append("Source framemd5 timestamps disagree with indexed source PTS")
    if not output_errors or max(output_errors) > PTS_TOLERANCE_US:
        failures.append("Export timestamps disagree with source-to-output keep mapping")
    mismatches = []
    mismatch_count = 0
    for i, (ref, out) in enumerate(zip(source_rows, output_rows)):
        if ref["md5"] != out["md5"] or ref["bytes"] != out["bytes"]:
            mismatch_count += 1
            if len(mismatches) < 20:
                mismatches.append({"outputIndex": i, "sourceUs": ref["ptsUs"], "outputUs": out["ptsUs"],
                                   "sourceHash": ref["md5"], "outputHash": out["md5"]})
    report["pixelEquality"] = {"mismatchFrames": mismatch_count, "firstMismatches": mismatches,
                               "allKeptFrameHashesEqual": mismatch_count == 0 and len(source_rows) == len(output_rows) == len(expected)}
    if not report["pixelEquality"]["allKeptFrameHashesEqual"]:
        failures.append("Source/output decoded pixel hashes differ; no perceptual threshold substituted")

    # Check all frozen protected source frames at their mapped output indices.
    source_to_index = {t: i for i, t in enumerate(expected_source)}
    protected = []
    original_pts = project["source"]["framePts"]
    for case in annotations["cases"]:
        if case["video"] != kind or not case["label"].startswith("preserve"):
            continue
        lo, hi = bisect.bisect_left(original_pts, case["startUs"]), bisect.bisect_left(original_pts, case["endUs"])
        missing, unequal, timing = [], [], []
        for t in original_pts[lo:hi]:
            index = source_to_index.get(t)
            if index is None or index >= len(source_rows) or index >= len(output_rows):
                missing.append(t)
                continue
            if source_rows[index]["md5"] != output_rows[index]["md5"]:
                unequal.append(t)
            if abs(output_rows[index]["ptsUs"] - expected_output[index]) > PTS_TOLERANCE_US:
                timing.append(t)
        row = {"id": case["id"], "label": case["label"], "sourceFrames": hi - lo,
               "missingFrames": len(missing), "pixelMismatchFrames": len(unequal), "timingMismatchFrames": len(timing),
               "firstMissingSourceUs": missing[:10], "firstPixelMismatchSourceUs": unequal[:10],
               "passed": not missing and not unequal and not timing and hi - lo == case.get("frameCount", hi - lo)}
        protected.append(row)
        if not row["passed"]:
            failures.append(f"Protected interval{case['id']} failed mapped output retention/hash/timing check")
    report["protectedFrames"] = {"cases": len(protected), "passedCases": sum(p["passed"] for p in protected),
                                 "frames": sum(p["sourceFrames"] for p in protected),
                                 "missingFrames": sum(p["missingFrames"] for p in protected),
                                 "pixelMismatchFrames": sum(p["pixelMismatchFrames"] for p in protected),
                                 "timingMismatchFrames": sum(p["timingMismatchFrames"] for p in protected), "checks": protected}

    # Read actual last encoded display packet duration, not nominal1/fps.
    seek = max(0, plan["durationUs"] - 1000000) / 1000000
    packet_meta = probe(args, movie, results_dir, "video-tail-packets",
                        ["-select_streams", "v:0", "-read_intervals", f"{seek:.6f}%", "-show_packets",
                         "-show_entries", "packet=pts_time,duration_time"])
    packets = [p for p in packet_meta.get("packets", []) if microseconds(p.get("pts_time")) is not None]
    if not packets:
        raise AssertionError("No final video packets available for duration validation")
    last_packet = max(packets, key=lambda p: microseconds(p["pts_time"]))
    last_pts, last_duration = microseconds(last_packet["pts_time"]), microseconds(last_packet.get("duration_time"))
    if not last_duration or last_duration <= 0:
        raise AssertionError("Final video packet has no positive display duration")
    video_end = last_pts + last_duration
    video_stream_end = (microseconds(video.get("start_time")) or 0) + (microseconds(video.get("duration")) or 0)
    format_duration = microseconds(metadata.get("format", {}).get("duration"))
    duration_error = video_end - plan["durationUs"]
    report["duration"] = {"videoLastPtsUs": last_pts, "videoLastPacketDurationUs": last_duration,
                          "actualVideoEndUs": video_end, "videoStreamEndUs": video_stream_end,
                          "formatDurationUs": format_duration, "videoEndErrorUs": duration_error,
                          "videoEndToleranceUs": VIDEO_DURATION_TOLERANCE_US}
    if output_rows and abs(last_pts - output_rows[-1]["ptsUs"]) > PTS_TOLERANCE_US:
        failures.append("Last encoded packet PTS does not match last decoded display frame")
    if abs(duration_error) > VIDEO_DURATION_TOLERANCE_US:
        failures.append("Actual video duration differs from keep plan beyond the predeclared tolerance")
    if format_duration is None or abs(format_duration - plan["durationUs"]) > AUDIO_TAIL_TOLERANCE_US:
        failures.append("Container duration differs from plan by more than40ms")

    audio_expected = project.get("audioIndex") is not None or bool(project.get("music"))
    if audio_expected and not audio_streams:
        failures.append("Expected game audio stream is missing")
    audio_checks = []
    for number, stream in enumerate(audio_streams):
        frames = probe(args, movie, results_dir, f"audio-{number}-frames",
                       ["-select_streams", f"a:{number}", "-show_frames", "-show_entries",
                        "frame=best_effort_timestamp_time,pts_time,nb_samples,sample_rate"])["frames"]
        sample_rate = int(stream["sample_rate"])
        entries = []
        for frame in frames:
            t = microseconds(frame.get("best_effort_timestamp_time", frame.get("pts_time")))
            n = frame.get("nb_samples")
            if t is None or n is None:
                raise AssertionError("Decoded audio frame missing timestamp/sample count")
            rate = int(frame.get("sample_rate", sample_rate))
            entries.append((t, round(int(n) * 1000000 / rate), int(n)))
        if not entries:
            raise AssertionError("Audio stream decoded zero frames")
        monotonic = all(a[0] < b[0] for a, b in zip(entries, entries[1:]))
        audio_end = entries[-1][0] + entries[-1][1]
        av_tail_delta = audio_end - video_end
        stream_end = (microseconds(stream.get("start_time")) or 0) + (microseconds(stream.get("duration")) or 0)
        ok = monotonic and abs(av_tail_delta) <= AUDIO_TAIL_TOLERANCE_US and abs(stream_end - video_end) <= AUDIO_TAIL_TOLERANCE_US
        audio_checks.append({"streamIndex": stream["index"], "codec": stream["codec_name"], "sampleRate": sample_rate,
                             "decodedFrames": len(entries), "decodedSamples": sum(x[2] for x in entries),
                             "ptsMonotonic": monotonic, "firstPtsUs": entries[0][0], "decodedEndUs": audio_end,
                             "streamEndUs": stream_end, "decodedAudioMinusVideoEndUs": av_tail_delta,
                             "streamAudioMinusVideoEndUs": stream_end - video_end,
                             "tailToleranceUs": AUDIO_TAIL_TOLERANCE_US, "passed": ok})
        if not ok:
            failures.append(f"Audio stream{stream['index']} PTS or A/V tail alignment failed")
    report["audio"] = audio_checks
    now_source, now_movie = source.stat(), movie.stat()
    unchanged = ((source_stat.st_size, source_stat.st_mtime_ns) == (now_source.st_size, now_source.st_mtime_ns)
                 and (movie_stat.st_size, movie_stat.st_mtime_ns) == (now_movie.st_size, now_movie.st_mtime_ns))
    report["sourceAndOutputUnchangedDuringReadOnlyCheck"] = unchanged
    if not unchanged:
        failures.append("Source or output file changed while validation was reading it")
    report["status"] = "passed" if not failures else "failed"
    save(results_dir / "summary.json", report)
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--run-dir", type=Path, default=DEFAULT_RUN)
    parser.add_argument("--annotations", type=Path, default=ANNOTATIONS)
    parser.add_argument("--videos", nargs="+", choices=("pc", "mobile"), default=["pc", "mobile"])
    parser.add_argument("--ffmpeg", type=Path, default=ROOT / "tools/media/ffmpeg.exe")
    parser.add_argument("--ffprobe", type=Path, default=ROOT / "tools/media/ffprobe.exe")
    parser.add_argument("--timeout", type=float, default=1800)
    args = parser.parse_args()
    args.run_dir = args.run_dir.resolve()
    missing = [flag for flag, location in (("--annotations", args.annotations), ("--ffmpeg", args.ffmpeg), ("--ffprobe", args.ffprobe)) if not location.is_file()]
    if missing:
        parser.error("Missing inputs: " + ", ".join(missing) + ". Supply explicit annotations and media-tool paths; private validation data is not included in source delivery.")
    annotations = read(args.annotations)
    target = args.run_dir / "output-check"
    target.mkdir(parents=True, exist_ok=True)
    summary = {"schemaVersion": 1, "status": "running", "annotationsSha256": sha_bytes(args.annotations.read_bytes()),
               "method": "Actual export keep plan; full source/output decode to yuv420p framemd5; real-PTS mapping; complete audio decode for tail timing",
               "tolerances": {"pixelHashMismatchFrames": 0, "ptsRescaleUs": PTS_TOLERANCE_US,
                              "videoDurationUs": VIDEO_DURATION_TOLERANCE_US, "audioTailUs": AUDIO_TAIL_TOLERANCE_US},
               "videos": []}
    began = time.monotonic()
    for kind in args.videos:
        try:
            report = verify_one(args, kind, annotations)
        except Exception as error:
            report = {"video": kind, "status": "failed", "failures": [str(error)]}
            save(target / kind / "summary.json", report)
        summary["videos"].append(report)
        save(target / "summary.json", summary)
        print(json.dumps({"video": kind, "status": report["status"], "failures": report.get("failures", []),
                          "decodedFrames": report.get("decodedFrames"),
                          "pixelMismatchFrames": report.get("pixelEquality", {}).get("mismatchFrames"),
                          "protectedFrames": {k: v for k, v in report.get("protectedFrames", {}).items() if k != "checks"}},
                         ensure_ascii=False), flush=True)
    summary["seconds"] = round(time.monotonic() - began, 3)
    summary["status"] = "passed" if all(v["status"] == "passed" for v in summary["videos"]) else "failed"
    summary["protectedFramesChecked"] = sum(v.get("protectedFrames", {}).get("frames", 0) for v in summary["videos"])
    summary["allProtectedCasesPassed"] = all(v.get("protectedFrames", {}).get("passedCases", -1) == v.get("protectedFrames", {}).get("cases", -2) for v in summary["videos"])
    save(target / "summary.json", summary)
    return 0 if summary["status"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(main())
