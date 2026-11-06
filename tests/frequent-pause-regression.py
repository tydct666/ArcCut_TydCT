"""Isolated real-video regression for dense pauses and protected gameplay.

Requires an imported baseline project and independently reviewed source-PTS
annotations. No source media or existing project is changed. Run from any cwd:
  python tests/frequent-pause-regression.py --baseline PROJECT --annotations JSON
Use a fresh --run-dir for each CLI build; --resume requires identical input hashes.
Outputs include native requests,
results, frozen CLI, current project, preview, full export and a final audit.
"""
from __future__ import annotations

import argparse
import bisect
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import time
import uuid

ROOT = Path(__file__).resolve().parents[1]
VERSION = "ui-events-8-transitions-3"


def read(path):
    return json.loads(Path(path).read_text(encoding="utf-8-sig"))


def save(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding="utf-8")


def sha(path):
    h = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def clone(project):
    p = copy.deepcopy(project)
    p["projectId"] = str(uuid.uuid4())
    p["revision"] = 1
    p.pop("completedExport", None)
    p.pop("outputPath", None)
    p["music"] = []
    p["mix"]["currentTrack"] = None
    return p


def removed_indices(project):
    pts = project["source"]["framePts"]
    removed = set()
    for pause in project["pauses"]:
        if pause["enabled"] and pause["state"] == "confirmed":
            removed.update(range(bisect.bisect_left(pts, pause["startUs"]),
                                 bisect.bisect_left(pts, pause["endUs"])))
    return removed


def verify_annotations(project, baseline, annotations):
    pts = project["source"]["framePts"]
    before, after = removed_indices(baseline), removed_indices(project)
    cases = []
    for case in annotations["cases"]:
        first = bisect.bisect_left(pts, case["startUs"])
        end = bisect.bisect_left(pts, case["endUs"])
        assert first == case["firstFrameIndex"] and end == case["endFrameIndexExclusive"], case["id"]
        indices = set(range(first, end))
        assert len(indices) == case["frameCount"] and indices, case["id"]
        must_remove = case["label"] == "removeHardPause"
        assert must_remove or case["label"].startswith("preserve"), case["label"]
        errors = indices - after if must_remove else indices & after
        cases.append({"id": case["id"], "label": case["label"], "frameCount": len(indices),
                      "beforeRemovedFrames": len(indices & before),
                      "afterRemovedFrames": len(indices & after), "errorIndices": sorted(errors),
                      "passed": not errors})
    special = set(annotations.get("specialProtectedFrameIndices", []))
    result = {"passed": all(c["passed"] for c in cases) and not (special & after),
              "cases": cases, "sourceFrameCount": len(pts),
              "beforeRemovedFrames": len(before), "afterRemovedFrames": len(after),
              "restoredFrameIndices": sorted(before - after),
              "newlyRemovedFrameIndices": sorted(after - before),
              "specialProtectedFrames": len(special),
              "specialBeforeRemovedFrames": len(special & before),
              "specialAfterRemovedFrames": len(special & after),
              "limits": annotations.get("verificationLimits", [])}
    return result


def frontend_merge(before, analysis, node):
    """Execute the existing production merge expression, rather than a replica."""
    path = ROOT / "src/main.tsx"
    source = path.read_text(encoding="utf-8")
    match = re.search(r"const next=clone\(before\),preserved=.*?;next\.pauses=.*?;(?=next\.detection=)", source)
    if not match:
        raise RuntimeError("Frontend analysis merge changed; update the extraction before running this test")
    code = ("const fs=require('fs');const {before,result}=JSON.parse(fs.readFileSync(0,'utf8'));"
            "const clone=structuredClone;" + match.group(0) + "process.stdout.write(JSON.stringify(next));")
    result = subprocess.run([node, "-e", code], input=json.dumps({"before": before, "result": analysis}),
                            text=True, encoding="utf-8", capture_output=True, check=True)
    return json.loads(result.stdout), {"source": str(path), "sourceSha256": sha(path),
                                     "executedProductionExpression": match.group(0)}


def clean_audit(audit):
    assert audit and audit.get("status") not in ("failed", "unsupported"), audit
    assert audit.get("residualFrames", 0) == 0, audit
    assert audit.get("transitionResidualFrames", 0) == 0, audit
    assert audit.get("transitionVersion") == VERSION and audit.get("checks"), audit
    return {key: audit.get(key) for key in ("profile", "transitionVersion", "needsReview", "residualFrames",
                                           "transitionResidualFrames", "candidateFrames")}


class Run:
    def __init__(self, args):
        self.args = args
        self.folder = args.run_dir
        self.summary = {"status": "running", "stages": [], "version": VERSION,
                        "baselineSha256": sha(args.baseline), "annotationsSha256": sha(args.annotations)}
        self.folder.mkdir(parents=True, exist_ok=True)
        self.cli = self.folder / "arkcut-cli.exe"
        if (self.folder / "summary.json").exists() or self.cli.exists():
            if not args.resume:
                raise FileExistsError("Run directory already contains a regression; choose a fresh --run-dir or --resume")
            old = read(self.folder / "summary.json")
            for key in ("baselineSha256", "annotationsSha256"):
                assert old[key] == self.summary[key], f"Resume input changed: {key}"
            assert old["cliSha256"] == sha(self.cli) == sha(args.cli), "Resume CLI changed"
            self.summary = old
            self.summary["status"] = "running"
            self.summary.pop("error", None)
        else:
            shutil.copy2(args.cli, self.cli)
        self.summary["cliSha256"] = sha(self.cli)
        self.env = dict(os.environ)
        for key in ("ARKCUT_RESOURCE_TEST_FILE", "ARKCUT_UPDATE_TEST_FILE", "ARKCUT_COMPONENT_TEST_FILE"):
            self.env.pop(key, None)
        for name, variable in (("data", "ARKCUT_DATA_DIR"), ("temp", "TEMP")):
            directory = self.folder / name
            directory.mkdir(exist_ok=True)
            self.env[variable] = str(directory)
        self.env["TMP"] = self.env["TEMP"]
        self.env["ARKCUT_MEDIA_DIR"] = str(args.media_dir)
        self.flush()

    def flush(self):
        save(self.folder / "summary.json", self.summary)

    def call(self, stage, op, payload):
        request = self.folder / f"{stage}-request.json"
        output = self.folder / f"{stage}-result.json"
        error = self.folder / f"{stage}-stderr.txt"
        if request.exists() and output.exists() and self.args.resume:
            assert read(request) == {"op": op, "payload": payload}, f"Resume request changed: {stage}"
            cached = read(output) if output.stat().st_size else {}
            if cached.get("job", {}).get("status") == "completed":
                print(f"{stage}: reused completed result from the same frozen CLI", flush=True)
                return cached["job"]["result"]
        save(request, {"op": op, "payload": payload})
        began = time.monotonic()
        with output.open("wb") as out, error.open("wb") as err:
            process = subprocess.Popen([str(self.cli), str(request)], env=self.env, stdout=out, stderr=err,
                                       creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
            try:
                while process.poll() is None:
                    try:
                        process.wait(timeout=20)
                    except subprocess.TimeoutExpired:
                        print(f"{stage}: {time.monotonic() - began:.0f}s elapsed", flush=True)
                        if time.monotonic() - began > self.args.timeout:
                            raise TimeoutError(stage)
            finally:
                if process.poll() is None:
                    # Stop only this test's own live child process tree.
                    if os.name == "nt":
                        subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"],
                                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
                    else:
                        process.kill()
                    process.wait(timeout=20)
        value = read(output) if output.stat().st_size else {}
        job = value.get("job", {})
        row = {"stage": stage, "exitCode": process.returncode, "status": job.get("status"),
               "seconds": round(time.monotonic() - began, 3), "error": job.get("error")}
        self.summary["stages"].append(row)
        self.flush()
        print(json.dumps(row, ensure_ascii=False), flush=True)
        if process.returncode or (job and job.get("status") != "completed"):
            raise RuntimeError(job.get("error") or error.read_text(encoding="utf-8", errors="replace"))
        return job.get("result", value)

    def execute(self):
        baseline = read(self.args.baseline)
        annotations = read(self.args.annotations)
        source = Path(baseline["source"]["fingerprint"]["path"])
        fingerprint = annotations["sourceFingerprint"]
        assert source.stat().st_size == fingerprint["sizeBytes"], "Annotations belong to a different source size"
        assert sha(source) == fingerprint["sha256"], "Annotations belong to different source bytes"
        self.summary["sourceSha256"] = fingerprint["sha256"]
        original_path = self.folder / "old-input.arkcut.json"
        original = read(original_path) if original_path.exists() else clone(baseline)
        save(self.folder / "old-input.arkcut.json", original)
        old_result = self.call("old-project-preview", "preview", {
            "project": original, "positionUs": self.args.position_us, "halfSeconds": 3})
        self.summary["oldProjectPreviewAudit"] = clean_audit(old_result["audit"])
        old_effective = old_result.get("effectiveProject") or original
        assert old_effective["pauses"] == original["pauses"], "Old preview unexpectedly changed boundaries"
        self.summary["oldProjectBoundsUnchanged"] = True
        self.summary["oldProjectLimitation"] = "Previously removed gameplay is restored only after explicit reanalysis."

        analysis_request = self.folder / "analyze-request.json"
        working = read(analysis_request)["payload"]["project"] if analysis_request.exists() else clone(baseline)
        analysis = self.call("analyze", "analyze", {"project": working})
        assert analysis["status"] == "completed", analysis["status"]
        save(self.folder / "analysis.json", analysis)
        current, merge_evidence = frontend_merge(working, analysis, self.args.node)
        current["detection"].update({key: analysis[key] for key in ("status", "geometry", "region")})
        current["detection"].update(profile=analysis["version"], version=VERSION)
        current["revision"] += 1
        save(self.folder / "current.arkcut.json", current)
        verification = verify_annotations(current, baseline, annotations)
        save(self.folder / "annotation-verification.json", verification)
        assert verification["passed"], "Independent annotation check failed"
        self.summary["annotationVerification"] = verification

        manual_path = self.folder / "manual-input.arkcut.json"
        manual = read(manual_path) if manual_path.exists() else clone(original)
        save(manual_path, manual)
        enabled = [p for p in manual["pauses"] if p["enabled"] and p["state"] == "confirmed"]
        assert len(enabled) >= 4, "Guard regression needs at least four baseline pauses"
        enabled[0]["source"] = "manual"
        enabled[1]["detectedBounds"] = [enabled[1]["startUs"] + 1, enabled[1]["endUs"]]
        preview_manual = copy.deepcopy(manual)
        enabled[2]["state"], enabled[2]["enabled"] = "rejected", False
        enabled[3]["enabled"] = False
        preserved = copy.deepcopy(enabled[:4])
        merged, _ = frontend_merge(manual, analysis, self.args.node)
        for pause in preserved:
            assert next(p for p in merged["pauses"] if p["id"] == pause["id"]) == pause
        save(self.folder / "manual-merge.arkcut.json", merged)
        manual_result = self.call("manual-project-preview", "preview", {
            "project": preview_manual, "positionUs": self.args.position_us, "halfSeconds": 3})
        assert (manual_result.get("effectiveProject") or preview_manual)["pauses"] == preview_manual["pauses"]
        self.summary["manualGuards"] = {"passed": True, "preservedTypes": ["manual", "editedAuto", "rejected", "disabledConfirmed"],
                                        "mergeEvidence": merge_evidence, "previewAudit": clean_audit(manual_result["audit"])}
        preview = self.call("current-preview", "preview", {
            "project": current, "positionUs": self.args.position_us, "halfSeconds": 3})
        self.summary["currentPreviewAudit"] = clean_audit(preview["audit"])
        current = preview.get("effectiveProject") or current
        target = self.folder / "processed.mp4"
        export = self.call("full-export", "export", {
            "project": current, "path": str(target), "formatChangeAccepted": True})
        current = export.get("effectiveProject") or current
        save(self.folder / "current.arkcut.json", current)
        self.summary["exportAudit"] = clean_audit(export["audit"])
        verification = verify_annotations(current, baseline, annotations)
        save(self.folder / "annotation-verification.json", verification)
        assert verification["passed"], "Post-export annotation check failed"
        self.summary["annotationVerification"] = verification
        audit = self.call("output-recheck", "checkOutput", {"project": current, "path": str(target)})
        self.summary["outputRecheckAudit"] = clean_audit(audit)
        command = [str(self.args.media_dir / "ffprobe.exe"), "-v", "error", "-count_frames", "-select_streams", "v:0",
                   "-show_entries", "stream=nb_read_frames,width,height:format=duration", "-of", "json", str(target)]
        probe = json.loads(subprocess.run(command, capture_output=True, text=True, encoding="utf-8", check=True).stdout)
        expected = len(current["source"]["framePts"]) - len(removed_indices(current))
        actual = int(probe["streams"][0]["nb_read_frames"])
        assert actual == expected, (actual, expected)
        self.summary["output"] = {"path": str(target), "sha256": sha(target), "bytes": target.stat().st_size,
                                  "expectedFrames": expected, "decodedFrames": actual, "probe": probe}
        assert sha(self.args.baseline) == self.summary["baselineSha256"]
        assert sha(self.args.annotations) == self.summary["annotationsSha256"]
        self.summary["baselineAndAnnotationsUnchanged"] = True
        self.summary["status"] = "passed"
        self.flush()


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--annotations", type=Path, required=True)
    parser.add_argument("--cli", type=Path, default=ROOT / "src-tauri/target/release/arkcut-cli.exe")
    parser.add_argument("--run-dir", type=Path, default=ROOT / "outputs/frequent-pause-validation/after")
    parser.add_argument("--media-dir", type=Path, default=ROOT / "tools/media")
    parser.add_argument("--node", default=shutil.which("node"))
    parser.add_argument("--position-us", type=int, default=339000)
    parser.add_argument("--timeout", type=float, default=1800)
    parser.add_argument("--resume", action="store_true", help="Reuse only completed native calls with identical CLI and inputs")
    args = parser.parse_args()
    for name in ("baseline", "annotations", "cli", "run_dir", "media_dir"):
        setattr(args, name, getattr(args, name).resolve())
    if not args.node:
        parser.error("Node.js is required to execute the production frontend merge; pass --node")
    run = Run(args)
    try:
        run.execute()
    except BaseException as error:
        run.summary["status"], run.summary["error"] = "failed", str(error)
        run.flush()
        raise


if __name__ == "__main__":
    main()
