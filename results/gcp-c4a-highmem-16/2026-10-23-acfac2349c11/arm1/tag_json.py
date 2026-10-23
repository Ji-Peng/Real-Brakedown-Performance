import sys, os, json

def tag_file(path, session_id, seed, watermark=None, freq_before=None, freq_after=None, block_id=None):
    try:
        with open(path, "r", encoding="utf-8") as f:
            data = json.load(f)
    except Exception:
        return
    modified = False
    if isinstance(data, list):
        for item in data:
            if isinstance(item, dict) and not item.get("block_id"):
                item["session_id"] = session_id
                item["session_seed"] = seed
                if block_id: item["block_id"] = block_id
                if freq_before: item["freq_before_mhz"] = freq_before
                if freq_after: item["freq_after_mhz"] = freq_after
                if watermark: item["watermark"] = watermark
                modified = True
    elif isinstance(data, dict) and not data.get("block_id"):
        data["session_id"] = session_id
        data["session_seed"] = seed
        if block_id: data["block_id"] = block_id
        if freq_before: data["freq_before_mhz"] = freq_before
        if freq_after: data["freq_after_mhz"] = freq_after
        if watermark: data["watermark"] = watermark
        modified = True
    if modified:
        with open(path, "w", encoding="utf-8") as f:
            json.dump(data, f, indent=2)

if __name__ == "__main__":
    target_dir = sys.argv[1]
    session_id = sys.argv[2]
    seed = sys.argv[3]
    watermark = sys.argv[4] if len(sys.argv) > 4 and sys.argv[4] else None
    freq_before = sys.argv[5] if len(sys.argv) > 5 and sys.argv[5] else None
    freq_after = sys.argv[6] if len(sys.argv) > 6 and sys.argv[6] else None
    block_id = sys.argv[7] if len(sys.argv) > 7 and sys.argv[7] else None

    for root, _, files in os.walk(target_dir):
        for fname in files:
            if fname.endswith(".json") and "manifest" not in fname and "schedule" not in fname:
                tag_file(os.path.join(root, fname), session_id, seed, watermark, freq_before, freq_after, block_id)
