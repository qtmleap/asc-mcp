"""Offline recipe verifier independent of the Rust evaluator."""
import json
from pathlib import Path
MISSING=object()
recipes=json.loads(Path('data/request_recipes.json').read_text());probes=json.loads(Path('data/go_request_probes.json').read_text())
def nonempty(v):return v is not None and v not in ('',[],{})
def string(v):return v if isinstance(v,str) else json.dumps(v,separators=(',',':'))
def sub(v,path):
 for p in path or []:
  try:v=v[int(p)] if isinstance(v,list) else v[p]
  except (KeyError,IndexError,TypeError,ValueError):return MISSING
 return v

def ev(n,a,item=None,index=0):
 op=n['op']; k=n.get('name');arg=a.get(k);arg=arg if arg is not None else MISSING
 if op=='literal':return n['value']
 if op=='arg':
  v=arg if arg is not MISSING else n.get('default',MISSING)
  if v is MISSING or (n.get('omit_false') and v is False):return MISSING
  if isinstance(v,int) and not isinstance(v,bool):
   if v<=0:v=n.get('nonpositive_default',v)
   if 'max' in n and isinstance(v,int):v=min(v,n['max'])
  if v=='':v=n.get('empty_default',v)
  v=sub(v,n.get('sub'));return string(v) if n.get('string') and v is not MISSING else v
 if op=='object':
  result={k:v for k,node in n['fields'].items() if (v:=ev(node,a,item,index)) is not MISSING}
  return MISSING if n.get('omit_if_empty') and not result else result
 if op=='array':return [v for node in n['items'] if (v:=ev(node,a,item,index)) is not MISSING]
 if op=='when_positive':return ev(n['value'],a,item,index) if (arg is MISSING and n.get('allow_missing')) or (arg is not MISSING and arg>0) else MISSING
 if op=='when':return ev(n['value'],a,item,index) if arg is not MISSING and (n.get('condition')=='present' or nonempty(arg)) else MISSING
 if op=='each':
  if arg==[] and 'empty_default' in n:return n['empty_default']
  return [ev(n['value'],a,v,i) for i,v in enumerate(arg)] if arg is not MISSING else n.get('default',[])
 if op=='item':
  value=sub(item,n.get('sub'));return MISSING if n.get('omit_empty') and not nonempty(value) else value
 if op=='item_format':return n['prefix']+string(item)+n['suffix']
 if op=='index':return n['prefix']+str(index+1)
 if op=='join':return n['separator'].join(map(string,arg)) if arg is not MISSING else MISSING
 if op=='format':return n['prefix']+string(arg)+n['suffix'] if arg is not MISSING else MISSING
 raise ValueError(op)
def build(name,args):
 r=recipes[name];s=r
 if r.get('supports_cursor') and args.get('cursor'):
  from urllib.parse import urlsplit,parse_qs
  cursor=urlsplit(args['cursor']);return {'method':'GET','path':cursor.path,'query':parse_qs(cursor.query),'body':None}
 for branch in r.get('branches',[]):
  if ('when_absent' in branch and not nonempty(args.get(branch['when_absent']))) or ('when' in branch and nonempty(args.get(branch['when'])) and ('equals' not in branch or string(args.get(branch['when'])).upper()==branch['equals'].upper())):s=branch;break
 result=ev(s['request'],args)
 for m in r.get('query_maps',[]):
  for k,v in (args.get(m) or {}).items():
   if v:
    key=m+'['+k+']'
    if key in r.get('query_map_overrides',[]):result['query'][key]=[','.join(v)]
    else:result['query'].setdefault(key,[','.join(v)])
 return result
if __name__=='__main__':
 failures=[];count=0
 for name,cases in probes.items():
  for label,p in cases.items():
   if not p['requests']:continue
   count+=1;actual=build(name,p['args']);expected=p['requests'][0]
   if actual!=expected:failures.append((name,label,expected,actual))
 print('requests',count,'failures',len(failures))
 for f in failures[:65]:print(*f,sep='\n')
 if failures:Path('migration/probe_failures.json').write_text(json.dumps(failures,indent=2))
 else:Path('migration/probe_failures.json').unlink(missing_ok=True)
