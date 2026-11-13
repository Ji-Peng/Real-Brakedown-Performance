import json, os, sys, datetime

def now():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

def load(path):
    try:
        with open(path, "r", encoding="utf-8") as f:
            data = json.load(f)
    except (OSError, ValueError):
        return None
    return data if isinstance(data, dict) else None

def save(path, data):
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=2)
        f.write("\n")
    os.replace(tmp, path)

env = os.environ
path = env["SESSION_SCHEDULE"]
with open(env["SCHEDULE_FILE"], "r", encoding="utf-8") as f:
    stage_sched = json.load(f)
data = load(path)
if data is None or "stages" not in data:
    legacy = data
    data = {
        "format": "rbd-session-schedule-v1",
        "session_id": stage_sched["session_id"],
        "profile": stage_sched["profile"],
        "host": env["SESS_HOST"],
        "run_id": env["SESS_RUN_ID"],
        "note": ("Append-only: one entry per all.sh invocation (stage) of this session; each stage's "
                 "full schedule, manifest and execution log are under its stage_dir. Output dirs named "
                 "<block>.prev-<UTC> hold an earlier stage's run of that block and are not analysed."),
        "stages": [],
    }
    if legacy is not None:
        data["legacy_schedule"] = legacy
profiles = data.setdefault("profiles", [])
if stage_sched["profile"] not in profiles:
    profiles.append(stage_sched["profile"])

tag = stage_sched["stage"]
entry = next((e for e in data["stages"] if e.get("stage") == tag), None)
if entry is None:
    entry = {"stage": tag}
    data["stages"].append(entry)

session_dir = env["SESS_DIR"]
def rel(p):
    return os.path.relpath(p, session_dir) if p else None

if sys.argv[1] == "begin":
    entry.update({
        "stage_label": stage_sched["stage_label"],
        "stage_dir": rel(env["SESS_STAGE_DIR"]),
        "started_utc": stage_sched["stage_started_utc"],
        "finished_utc": None,
        "profile": stage_sched["profile"],
        "seed": stage_sched["seed"],
        "code_sha": env["SESS_CODE_SHA"],
        "instance": env["SESS_INSTANCE"],
        "arguments": stage_sched["arguments"],
        "block_ids": [b["id"] for b in stage_sched["blocks"]],
        "blocks": [{"id": b["id"], "out": rel(b["out"]), "threads": b["threads"], "cores": b["cores"],
                    "siblings": b.get("siblings", []), "status": "scheduled"} for b in stage_sched["blocks"]],
        "exit_status": None,
        "state": "running",
    })
else:
    rc = int(sys.argv[2])
    state = sys.argv[3] if len(sys.argv) > 3 else "finished"
    log = {}
    try:
        with open(env["SESS_EXEC_LOG"], "r", encoding="utf-8") as f:
            for line in f:
                if line.strip():
                    item = json.loads(line)
                    log[item["block_id"]] = item
    except (OSError, ValueError):
        pass
    moved = {}
    try:
        with open(env["SESS_MOVED_TSV"], "r", encoding="utf-8") as f:
            for line in f:
                parts = line.rstrip("\n").split("\t")
                if len(parts) == 3:
                    moved[parts[0]] = parts[2]
    except OSError:
        pass
    for b in entry.get("blocks", []):
        item = log.get(b["id"])
        if item:
            b["status"] = item["status"]
            b["exit_code"] = item["exit_code"]
            b["duration_s"] = item["duration_s"]
        elif b.get("status") == "scheduled":
            b["status"] = "not_run"
        if b["id"] in moved:
            b["moved_previous_to"] = rel(moved[b["id"]])
    entry["finished_utc"] = now()
    entry["exit_status"] = rc
    entry["state"] = state
    entry["failed_blocks"] = [b["id"] for b in entry.get("blocks", []) if b.get("status") == "failed"]
save(path, data)
