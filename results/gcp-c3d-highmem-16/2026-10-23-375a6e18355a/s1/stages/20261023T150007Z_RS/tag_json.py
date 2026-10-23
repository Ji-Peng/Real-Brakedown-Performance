import json, os, sys

# Legacy This Work binaries (before bench/src/lib.rs recorded cpu_mhz_source)
# computed cycles_per_mac from the same reading.
LEGACY_MHZ_SOURCE = "/proc/cpuinfo first 'cpu MHz' entry (CPU 0); nominal, not the pinned core's measured clock"

def tag_item(item, fields):
    if not isinstance(item, dict):
        return False
    item.update({k: v for k, v in fields.items() if v is not None})
    # cycles_per_mac comes from the benchmark's own /proc/cpuinfo MHz reading
    # (cpu_mhz_source), never from the block-level freq_* fields above, so it
    # is always nominal: flag it from that same source, on every host.
    if item.get("cycles_per_mac") is not None:
        item["cycles_per_mac_is_nominal"] = True
        item.setdefault("cpu_mhz_source", LEGACY_MHZ_SOURCE)
    return True

def tag_file(path, fields):
    try:
        with open(path, "r", encoding="utf-8") as f:
            data = json.load(f)
    except Exception:
        return
    items = data if isinstance(data, list) else [data]
    modified = False
    for item in items:
        modified = tag_item(item, fields) or modified
    if modified:
        with open(path, "w", encoding="utf-8") as f:
            json.dump(data, f, indent=2)

if __name__ == "__main__":
    (target_dir, session_id, seed, watermark, freq_before, freq_after, freq_source,
     block_id, block_status, stage_tag) = sys.argv[1:11]
    nominal = freq_source.startswith("nominal")
    fields = {
        "session_id": session_id,
        "session_seed": seed,
        "stage": stage_tag,
        "block_id": block_id,
        "block_status": block_status,
        "freq_before_mhz": freq_before or None,
        "freq_after_mhz": freq_after or None,
        "freq_source": freq_source,
        "freq_is_nominal": nominal,
        "watermark": watermark or None,
    }
    if not os.path.isdir(target_dir):
        sys.exit(0)
    for root, _, files in os.walk(target_dir):
        for fname in files:
            if fname.endswith(".json") and "manifest" not in fname and "schedule" not in fname:
                tag_file(os.path.join(root, fname), fields)
