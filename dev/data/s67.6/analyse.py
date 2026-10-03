"""S67.6 on the cloud box: medians, gates and the two verdicts of S67.1's rule,
CPU in place of instructions (no PMU), three deciding cells at cap 1.

Usage, from this directory: tar -xJf requests.tar.xz && python3 analyse.py ."""
import csv, statistics, sys, os
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', '..', 'tools'))
from paired_excess import read, paired
S = sys.argv[1]
rows = list(csv.DictReader(open(f'{S}/cells.csv')))
REQ = f'{S}/web-requests'
loads = ['web-arena-40k', 'web-arena-150k', 'web-heap']
arms = ['D', 'HG', 'bestD', 'bestHG']
def cell(arm, load): return [r for r in rows if r['arm']==arm and r['load']==load]
def i(r,k): return int(float(r[k]))
def cpu(r): return (i(r,'mutator_cpu_in_the_loop_us')+i(r,'collector_cpu_at_the_stop_us'))/max(1,i(r,'iterations'))
def cpu_drain(r): return (i(r,'mutator_cpu_us')+i(r,'collector_cpu_us'))/max(1,i(r,'iterations'))
def coll(r): return i(r,'collector_cpu_from_the_start_us')/max(1,i(r,'iterations'))
def med(rs,f): return statistics.median(f(r) for r in rs)
def spread(rs,f):
    v=[f(r) for r in rs]; m=statistics.median(v); return (max(v)-min(v))/m if m else 0
def retained(r): return i(r,'web_retained_blocks_at_the_stop')-i(r,'web_retained_blocks_of_live_writes')
def pexcess(base, new, load):
    out=[]
    for rep in range(1,6):
        a=f'{REQ}/{base}-cap1-{load}-{rep}.csv'; b=f'{REQ}/{new}-cap1-{load}-{rep}.csv'
        if os.path.exists(a) and os.path.exists(b):
            fa,fs,_,_,_=paired(read(a),read(b)); out.append((fa/1e6,fs/1e6))
    return (statistics.median(x[0] for x in out), statistics.median(x[1] for x in out), len(out)) if out else None
print('per arm, medians (n): cpu ms/req loop | +drain | collector ms/req | gmean MB | gpeak MB | gend KB | retained blk | tokwait ms | p99.9 arr ms | void cells')
for load in loads:
    print(f'== {load}')
    for arm in arms:
        rs=cell(arm,load)
        if not rs: continue
        print(f'  {arm:7s} n={len(rs)} cpu {med(rs,cpu)/1e3:.3f} (spread {spread(rs,cpu):.1%}) +drain {med(rs,cpu_drain)/1e3:.3f} coll {med(rs,coll)/1e3:.3f} '
              f'gmean {med(rs,lambda r:i(r,"web_garbage_mean_bytes"))/1e6:.2f} gpeak {med(rs,lambda r:i(r,"web_garbage_peak_bytes"))/1e6:.2f} '
              f'gend {max(i(r,"web_garbage_at_the_drain_end_bytes") for r in rs)/1e3:.1f}max retained {med(rs,retained):.0f} '
              f'tokwait {med(rs,lambda r:i(r,"token_wait_longest_us"))/1e3:.2f} p999 {med(rs,lambda r:i(r,"arrival_latency_p999_ns"))/1e6:.1f} '
              f'void {sum(r["void"]=="1" for r in rs)} other {max(float(r["other_cpu_cores"]) for r in rs):.2f}max')
def compare(base, new, title):
    print(f'\n### {title}: {new} against {base}')
    wins=losses=0; gates_fail=[]
    for load in loads:
        b=cell(base,load); n=cell(new,load)
        if not b or not n: continue
        tol=max(0.03, 2*spread(b,cpu))
        ch=med(n,cpu)/med(b,cpu)-1
        v='WIN' if ch<-tol else 'LOSS' if ch>tol else 'tie'
        wins+=v=='WIN'; losses+=v=='LOSS'
        g=[]
        def gate(name, nv, bv, ratio, floor):
            ok = nv <= ratio*bv+floor
            g.append(f'{name} {"ok" if ok else "FAIL"} ({nv:.0f} vs {ratio}x{bv:.0f}+{floor})')
            return ok
        gate('gmean', med(n,lambda r:i(r,'web_garbage_mean_bytes')), med(b,lambda r:i(r,'web_garbage_mean_bytes')),1.10,65536)
        gate('gpeak', med(n,lambda r:i(r,'web_garbage_peak_bytes')), med(b,lambda r:i(r,'web_garbage_peak_bytes')),1.25,262144)
        gate('retained', med(n,retained), med(b,retained),1.10,4)
        gate('tokwait', med(n,lambda r:i(r,'token_wait_longest_us')), med(b,lambda r:i(r,'token_wait_longest_us')),2,1000)
        pe=pexcess(base,new,load)
        if pe: g.append(f'paired p99.9 {pe[0]:.2f} ms arrival / {pe[1]:.2f} ms service over {pe[2]} {"ok" if pe[0]<=1 else "FAIL"}')
        dn=all(i(r,'web_garbage_at_the_drain_end_bytes')==0 for r in n); db=all(i(r,'web_garbage_at_the_drain_end_bytes')==0 for r in b)
        g.append(f'drain-freed new {"ok" if dn else "FAIL"} base {"ok" if db else "FAIL"}')
        print(f'  {load}: cpu {ch:+.1%} tol {tol:.1%} -> {v}; '+'; '.join(g))
    print(f'  wins {wins}, losses {losses}')
compare('bestD','bestHG','Verdict 1 (best HG vs best D)')
compare('D','HG','reported: HG vs D')
compare('D','bestD','Verdict 2 candidate (best D vs D)')
compare('HG','bestHG','Verdict 2 candidate (best HG vs HG)')
