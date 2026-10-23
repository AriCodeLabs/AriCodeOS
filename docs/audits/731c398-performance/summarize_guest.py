#!/usr/bin/env python3
"""Summarize raw audit serial logs without changing or filtering failures."""
from pathlib import Path
import json, re
HERE=Path(__file__).resolve().parent
out={}
for cpu in (1,4):
    source=HERE/f'guest-{cpu}cpu.log'
    text=source.read_text()
    runs=[]
    for block in text.split('bench: calibration ')[1:]:
        metrics={}
        for name,n,p50,unit,p95,p99 in re.findall(r'bench: (.*?) \((\d+)\): p50 (\d+) (ns|us) p95 (\d+) \w+ p99 (\d+) \w+',block):
            metrics[name]={'samples':int(n),'unit':unit,'p50':int(p50),'p95':int(p95),'p99':int(p99)}
        control=re.search(r'audit: busy/sleep elapsed cycles (\d+) busy cycles (\d+)',block)
        raw_idle=[{'cpu':int(c),'permille':int(p),'cycles':int(v)} for c,p,v in re.findall(r'audit: busy/sleep CPU (\d+) idle permille (\d+) idle cycles (\d+)',block)]
        entry={'metrics':metrics,'quiet_idle_permille':[int(p) for p in re.findall(r'audit: quiet CPU \d+ idle permille (\d+)',block)],'control_idle':raw_idle}
        if control:
            elapsed,busy=map(int,control.groups())
            entry.update(control_elapsed_cycles=elapsed,control_busy_cycles=busy,
                control_idle_upper_bound_per_cpu_average_percent=100*(cpu*elapsed-busy)/(cpu*elapsed),
                control_reported_idle_average_percent=100*sum(r['cycles'] for r in raw_idle)/(cpu*elapsed))
        runs.append(entry)
    assert len(runs)==2, (cpu,len(runs))
    assert all(len(r['metrics'])==5 for r in runs)
    out[f'{cpu}cpu']=runs
(HERE/'guest-summary.json').write_text(json.dumps(out,indent=2)+'\n')
for cpu,runs in out.items():
    r=runs[1]
    print(cpu,'warm metrics:',r['metrics'])
    print('Idle control reported %.2f%% versus upper bound %.2f%% (excluding small observation overhead)' % (r['control_reported_idle_average_percent'],r['control_idle_upper_bound_per_cpu_average_percent']))
