import re, os, bisect, sys
ROOT = os.getcwd()
RFC_ROOT = "/home/edmond/limelight"
FILES = []
for base in ["src", "benches", "docs"]:
    for dirpath, _, filenames in os.walk(os.path.join(ROOT, base)):
        for fn in filenames:
            FILES.append(os.path.join(dirpath, fn))
FILES += [os.path.join(ROOT, "dev/INDEX.md"), os.path.join(ROOT, "dev/ARCHITECTURE.md")]
PATH_RE = re.compile(r'`([a-zA-Z0-9_./-]+\.md)`')
def norm(s):
    s = s.replace('*', '').replace('_', '')
    return re.sub(r'\s+', ' ', s).strip()
cache = {}
def read(path):
    if path not in cache:
        try:
            cache[path] = norm(open(path, encoding='utf-8').read())
        except OSError:
            cache[path] = None
    return cache[path]
def resolve(path):
    return os.path.join(RFC_ROOT, path) if path.startswith("rfc/") else os.path.join(ROOT, path)
misses = total = 0
for fpath in sorted(FILES):
    try:
        text = open(fpath, encoding='utf-8').read()
    except OSError:
        continue
    line_starts = [0] + [i + 1 for i, c in enumerate(text) if c == '\n']
    seen = [(m.start(), m.group(1)) for m in PATH_RE.finditer(text)]
    for m in PATH_RE.finditer(text):
        path, start = m.group(1), m.end()
        window = text[start:start + 40]
        qm = re.search(r'"', window)
        if not qm or qm.start() > 15 or re.search(r'S\d+(\.\d+)*', window[:qm.start()]):
            continue
        q_start = start + qm.start() + 1
        close = text.find('"', q_start)
        if close == -1 or close - q_start > 400:
            continue
        quoted = norm(' '.join(re.sub(r'^(///|//!|//|\*|>)\s?', '', l.strip())
                                for l in text[q_start:close].split('\n')).rstrip('\\'))
        resolved = path
        if '/' not in path and not os.path.exists(os.path.join(ROOT, path)) \
           and not os.path.exists(os.path.join(RFC_ROOT, path)):
            for off, p in seen:
                if off >= m.start():
                    break
                if '/' in p and p.endswith('/' + path):
                    resolved = p
        content = read(resolve(resolved))
        total += 1
        ok = content is not None and quoted.replace('`', '') in content.replace('`', '')
        if not ok:
            misses += 1
            ln = bisect.bisect_right(line_starts, m.start())
            print(f"{fpath[len(ROOT)+1:]}:{ln}\t{path}\t{quoted}")
print(f"# total={total} misses={misses}", file=sys.stderr)
