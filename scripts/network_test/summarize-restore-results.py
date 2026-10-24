#!/usr/bin/env python3
import json,statistics,pathlib,argparse
parser=argparse.ArgumentParser(description="Summarize downloaded restore research JSONL files")
parser.add_argument("directory",type=pathlib.Path)
parser.add_argument("--output",type=pathlib.Path)
args=parser.parse_args()
base=args.directory
output=args.output or base
output.mkdir(parents=True,exist_ok=True)
rows=[]
for name in ['results.jsonl','extra-results.jsonl','actual-queue-results.jsonl','calibration-results.jsonl']:
 p=base/name
 if p.exists():rows += [json.loads(l) for l in p.read_text().splitlines() if l.strip()]
summary={'samples':len(rows),'verified':sum(r['result'].get('verified',False) for r in rows),'host':[],'steady':[],'handles':[],'queue':[],'tape':[],'extra':[],'batch':[]}
lines=['## 主机真实输出管线：1GiB筛选，3次中位数','','|文件大小|目标|总MB/s|数据+排空MB/s|准备秒|重开秒|关闭秒|同步秒|','|---|---|---:|---:|---:|---:|---:|---:|']
for size in [1048576,4194304,16777216,67108864]:
 for proto in ['local','nfs','smb']:
  a=[r['result'] for r in rows if r['tag'].startswith(f'host-{proto}-{size}-')]
  if not a:continue
  med=lambda f:statistics.median(f(v) for v in a)
  d=dict(size=size,protocol=proto,n=len(a),total_MB_s=med(lambda v:v['bytes']/v['total_seconds']/1e6),data_MB_s=med(lambda v:v['bytes']/(v['submit_seconds']+v['drain_sync_seconds'])/1e6),prepare=med(lambda v:v['prepare_seconds']),reopen=med(lambda v:v['operations'].get('reopen',{}).get('total_seconds',0)),close=med(lambda v:v['operations']['close_final']['total_seconds']),sync=med(lambda v:v['operations']['syncfs']['total_seconds']))
  summary['host'].append(d);lines.append(f"|{size//1048576}MiB|{proto}|{d['total_MB_s']:.2f}|{d['data_MB_s']:.2f}|{d['prepare']:.3f}|{d['reopen']:.3f}|{d['close']:.3f}|{d['sync']:.3f}|")
lines+=['','## 8GiB持续主机输出','','|文件大小|目标|总MB/s|数据+排空MB/s|准备秒|重开秒|关闭秒|','|---|---|---:|---:|---:|---:|---:|']
for r in rows:
 v=r['result'];tag=r['tag']
 if tag.startswith('steady'):
  d=dict(tag=tag,total_MB_s=v['bytes']/v['total_seconds']/1e6,data_MB_s=v['bytes']/(v['submit_seconds']+v['drain_sync_seconds'])/1e6,prepare=v['prepare_seconds'],reopen=v['operations'].get('reopen',{}).get('total_seconds',0),close=v['operations']['close_final']['total_seconds'])
  summary['steady'].append(d);lines.append(f"|{v['spec']['size']//1048576}MiB|{tag.split('-')[1]}|{d['total_MB_s']:.2f}|{d['data_MB_s']:.2f}|{d['prepare']:.3f}|{d['reopen']:.3f}|{d['close']:.3f}|")
 if tag.startswith('handles'):
  summary['handles'].append(dict(tag=tag,total=v['total_seconds'],prepare=v['prepare_seconds'],reopen_count=v['operations'].get('reopen',{}).get('count',0),reopen_seconds=v['operations'].get('reopen',{}).get('total_seconds',0)))
 if tag.startswith('queue'):summary['queue'].append(v)
 if tag.startswith('extra'):
  summary['extra'].append(dict(tag=tag,total_MB_s=v['bytes']/v['total_seconds']/1e6,prepare=v['prepare_seconds'],reopen=v['operations'].get('reopen',{}).get('total_seconds',0),write=v['operations']['write']['total_seconds'],close=v['operations']['close_final']['total_seconds']))
lines+=['','## 真实磁带恢复：8GiB/项','','|测试|读带跨度MB/s|恢复总秒|计划秒|准备秒|重开秒|关闭秒|READ数|LOCATE数|发送累计秒|缓冲获取累计秒|READ间隙P99 us|','|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|']
for r in rows:
 v=r['result'];tag=r['tag']
 if not tag.startswith(('null-','tape-')):continue
 o=v['operations'];span=o.get('read_span',{}).get('span_seconds');sec=lambda k:o.get(k,{}).get('total_seconds',0)
 d=dict(tag=tag,read_MB_s=v['bytes']/span/1e6 if span else None,restore_seconds=v['restore_seconds'],planning_seconds=v['planning_seconds'],prepare=sec('prepare'),reopen=sec('reopen'),close=sec('close_final'),read_count=o.get('tape_read',{}).get('count'),locate_count=o.get('tape_locate',{}).get('count'),send_seconds=sec('queue_send'),acquire_seconds=sec('acquire_buffer'),gap_p99_us=o.get('between_reads',{}).get('p99_us'),gap_max_ms=o.get('between_reads',{}).get('max_ms'),first_read_seconds=o.get('read_span',{}).get('first_start_seconds'))
 summary['tape'].append(d)
 lines.append(f"|{tag}|{d['read_MB_s'] or 0:.2f}|{d['restore_seconds']:.2f}|{d['planning_seconds']:.2f}|{d['prepare']:.3f}|{d['reopen']:.3f}|{d['close']:.3f}|{d['read_count']}|{d['locate_count']}|{d['send_seconds']:.3f}|{d['acquire_seconds']:.3f}|{d['gap_p99_us']}|")
lines+=['','## 连续32GiB恢复：每种文件大小8GiB','','|目标|完整读带跨度MB/s|恢复总秒|计划秒|准备秒|重开秒|关闭秒|READ数|LOCATE数|发送累计秒|缓冲获取累计秒|','|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|']
for r in rows:
 v=r['result'];tag=r['tag']
 if not tag.startswith('batch-'):continue
 o=v['operations'];span=o['read_span'];sec=lambda k:o.get(k,{}).get('total_seconds',0)
 samples=v['samples']
 points=[(0,span['first_start_seconds'])]+[(x['read_bytes'],x['seconds']) for x in samples]+[(v['bytes'],span['last_end_seconds'])]
 def at(byte):
  for (a,t),(b,u) in zip(points,points[1:]):
   if a<=byte<=b and b>a:return t+(u-t)*(byte-a)/(b-a)
  raise ValueError(byte)
 groups=[]
 for i,size in enumerate([1048576,4194304,16777216,67108864]):
  # Interior7GiB excludes the first/last512MiB of each8GiB group.
  a=i*8*1024**3+512*1024**2;b=(i+1)*8*1024**3-512*1024**2
  groups.append(dict(size=size,interior_MB_s=(b-a)/(at(b)-at(a))/1e6,interior_seconds=at(b)-at(a)))
 d=dict(tag=tag,read_MB_s=v['bytes']/span['span_seconds']/1e6,restore_seconds=v['restore_seconds'],planning_seconds=v['planning_seconds'],prepare=sec('prepare'),reopen=sec('reopen'),close=sec('close_final'),read_count=o['tape_read']['count'],locate_count=o['tape_locate']['count'],send_seconds=sec('queue_send'),acquire_seconds=sec('acquire_buffer'),groups=groups,first_read_seconds=span['first_start_seconds'],gap_p99_us=o['between_reads']['p99_us'],gap_max_ms=o['between_reads']['max_ms'],max_queued=max(x.get('queued',0) or 0 for x in samples),samples_waiting=sum(bool(x.get('waiting')) for x in samples),samples=len(samples))
 summary['batch'].append(d)
 lines.append(f"|{tag}|{d['read_MB_s']:.2f}|{d['restore_seconds']:.2f}|{d['planning_seconds']:.2f}|{d['prepare']:.3f}|{d['reopen']:.3f}|{d['close']:.3f}|{d['read_count']}|{d['locate_count']}|{d['send_seconds']:.3f}|{d['acquire_seconds']:.3f}|")
lines+=['','|目标|1MiB文件|4MiB文件|16MiB文件|64MiB文件|','|---|---:|---:|---:|---:|']
for d in summary['batch']:
 lines.append('|'+d['tag']+'|'+ '|'.join(f"{g['interior_MB_s']:.2f}" for g in d['groups'])+'|')
lines+=['','以上为每组中间7GiB窗口的十进制MB/s。']
lines+=['','## 句柄边界：真实管线总时间（秒）','','|文件大小|文件数|local|NFS|SMB|重开次数|','|---|---:|---:|---:|---:|---:|']
for size in [4096,1048576]:
 for count in [255,256,257,1024]:
  a=[]
  for proto in ['local','nfs','smb']:
   v=next((r['result'] for r in rows if r['tag']==f'handles-{proto}-{size}-{count}'),None)
   a.append(v['total_seconds'] if v else 0)
  lines.append(f"|{size}|{count}|{a[0]:.4f}|{a[1]:.4f}|{a[2]:.4f}|{max(0,count-256)}|")
lines+=['','## 实际spawn暂停输出的有效缓冲','','|文件大小|record|挂起payload字节|buffer数|配置MiB|','|---|---|---:|---:|---:|']
for r in rows:
 if r['tag'].startswith('queue-actual-'):
  v=r['result'];lines.append(f"|{v['spec']['size']}|{v['spec']['record']}|{v['snapshot']['queued_bytes']}|{v['allocated_buffers']}|512|")
lines+=['','## 8GiB/1MiB文件：平铺与分目录','','|目标|布局|总MB/s|准备秒|重开秒|','|---|---|---:|---:|---:|']
for proto in ['local','nfs','smb']:
 for tag,layout in [(f'steady-{proto}-1048576','平铺'),(f'extra-{proto}-nested-1m-8g','分目录')]:
  v=next((r['result'] for r in rows if r['tag']==tag),None)
  if v:lines.append(f"|{proto}|{layout}|{v['bytes']/v['total_seconds']/1e6:.2f}|{v['prepare_seconds']:.3f}|{v['operations'].get('reopen',{}).get('total_seconds',0):.3f}|")
lines+=['','## 相同1MiB文件、不同主机写入粒度：1GiB输出','','|目标|record|观测开关|总MB/s|','|---|---:|---|---:|']
for proto in ['local','nfs','smb']:
 for record,tag in [(4096,f'extra-{proto}-record-4k'),(65536,f'extra-{proto}-record-64k')]:
  v=next((r['result'] for r in rows if r['tag']==tag),None)
  if v:lines.append(f"|{proto}|{record}|开启|{v['bytes']/v['total_seconds']/1e6:.2f}|")
 for enabled in [False,True]:
  v=next((r['result'] for r in rows if r['tag']==f'calibration-4k-{proto}-{enabled}'),None)
  if v:lines.append(f"|{proto}|4096|{'开启校准' if enabled else '关闭校准'}|{v['bytes']/v['total_seconds']/1e6:.2f}|")
(output/'tables.md').write_text('\n'.join(lines)+'\n')
(output/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
print('samples',len(rows),'tape',len(summary['tape']),'extra',len(summary['extra']))
print('\n'.join(lines[-min(8,len(summary['tape'])):]) if summary['tape'] else '')
