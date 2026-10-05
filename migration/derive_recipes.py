"""Compile offline HTTP probes into a data-only request recipe language.

This is migration tooling, never invoked by the Rust runtime. Every generated
recipe is replayed against captured requests by the Rust differential test.
"""
import json, re
from pathlib import Path
PROBES=json.loads(Path('data/go_request_probes.json').read_text())
ACTUAL=json.loads(Path('data/handler_arguments.json').read_text())
TOOLS=json.loads(Path('data/public_tools.json').read_text())
MISSING=object()
COMPOUND={'upload_app_screenshot','upload_app_preview','upload_review_attachment','upload_build_file','upload_background_asset_file','upload_game_center_leaderboard_set_image','upload_game_center_activity_image','upload_game_center_challenge_image','get_sales_report','get_finance_report'}

def get(v,path):
 for part in path:
  try:v=v[int(part)] if isinstance(v,list) else v[part]
  except (KeyError,IndexError,TypeError,ValueError):return MISSING
 return v

def request(p):return p['requests'][0] if p.get('requests') else None

def lit(v):return {'op':'literal','value':v}

def compile_recipe(name,base,ps,properties):
 b=request(base);args=dict(sorted(base['args'].items(),key=lambda kv:-len(kv[0])));used=set()
 def counterpart(label,path):
  r=request(ps.get(label,{}));return get(r,path) if r is not None else MISSING
 def arg_node(k,path,sub=None,string=False):
  used.add(k);node={'op':'arg','name':k}
  if sub:node['sub']=sub
  if string:node['string']=True
  default=counterpart('omit:'+k,path)
  if default is not MISSING:node['default']=default
  empty=counterpart('empty:'+k,path)
  if empty is not MISSING and isinstance(args.get(k),str) and empty!='':node['empty_default']=empty
  if properties[k]['type'] in ('integer','number'):
   zero=counterpart('zero:'+k,path); high=counterpart('high:'+k,path)
   if zero is not MISSING and zero not in (0,'0'):node['nonpositive_default']=zero
   if high is not MISSING and high not in (1001,'1001'):node['max']=int(high)
  return node
 def wrap(node,path):
  if path[:1]==['query'] and len(path)==2 and path[1]=='limit' and request(ps.get('zero:limit',{})) is not None and counterpart('zero:limit',path) is MISSING:
   return {'op':'when_positive','name':'limit','allow_missing':counterpart('omit:limit',path) is not MISSING,'value':node}
  # Guard at the highest subtree removed by Go's omitempty or handler logic.
  for k in args:
   r=request(ps.get('omit:'+k,{}))
   if r is not None and get(r,path) is MISSING and (not path or get(r,path[:-1]) is not MISSING):
    used.add(k);return {'op':'when','name':k,'condition':'present' if counterpart('empty:'+k,path) is not MISSING else 'nonempty','value':node}
  return node
 def walk(v,path,item=None):
  # Exact arrays and maps preserve arbitrary lengths/keys.
  if isinstance(v,(list,dict)) and v:
   for k,a in args.items():
    if v==a and properties.get(k,{}).get('type') in ('array','object'):
     return wrap(arg_node(k,path),path)
  if isinstance(v,dict):
   node={'op':'object','fields':{k:walk(x,path+[k],item) for k,x in v.items()}}
   if item and path[-1:] == ['attributes'] and name in ('set_app_price_schedule','set_in_app_purchase_price_schedule'):node['omit_if_empty']=True
  elif isinstance(v,list):
   # JSON:API to-many relationships and included inline resources.
   text=json.dumps(v)
   field=None
   for k,a in args.items():
    if isinstance(a,list) and path[:1]!=['query'] and (f'MIG_{k}_0' in text or f'MIG_{k}.' in text):field=k;break
   if field is None and 'prices' in args and v and isinstance(v[0],dict) and v[0].get('id')=='price-1':field='prices'
   if field:
    used.add(field)
    node={'op':'each','name':field,'value':walk(v[0],path+['0'],field)}
    d=counterpart('omit:'+field,path)
    if d is not MISSING:node['default']=d
    e=counterpart('empty:'+field,path)
    if e is not MISSING:node['empty_default']=e
   else:node={'op':'array','items':[walk(x,path+[str(i)],item) for i,x in enumerate(v)]}
  elif isinstance(v,str):
   if item and v=='price-1':node={'op':'index','prefix':'price-'}
   elif item and f'MIG_{item}_0' in v:
    if v==f'MIG_{item}_0':node={'op':'item'}
    else:node={'op':'item_format','prefix':v.split(f'MIG_{item}_0')[0],'suffix':v.split(f'MIG_{item}_0')[1]}
   elif item and v.startswith(f'MIG_{item}.'):
    subkey=v.split('.',1)[1];node={'op':'item','sub':[subkey]}
    r=request(ps.get('nested_empty:'+item+':'+subkey,{}))
    if r is not None and get(r,path) is MISSING:node['omit_empty']=True
   else:
    found=False
    for k,a in args.items():
     if isinstance(a,list) and all(isinstance(x,str) for x in a) and v==','.join(a):node={'op':'join','name':k,'separator':','};used.add(k);found=True;break
     if isinstance(a,str) and a and a in v and (a.startswith(('MIG_','ALT_')) or counterpart('alt:'+k,path)==v.replace(a,'ALT_'+k)):
      if v==a:node=arg_node(k,path)
      else:node={'op':'format','name':k,'prefix':v.split(a,1)[0],'suffix':v.split(a,1)[1]};used.add(k)
      if path==['path']:node['encode_path']=True
      found=True;break
    if not found:
     node=lit(v)
     for k,a in args.items():
      alt=ps.get('alt:'+k,{}).get('args',{}).get(k,MISSING)
      av=counterpart('alt:'+k,path)
      if av is not MISSING and av!=v and av==str(alt) and v==str(a):node=arg_node(k,path,string=True);break
  else:
   node=lit(v)
   for k,a in args.items():
    alt=ps.get('alt:'+k,{}).get('args',{}).get(k,MISSING);av=counterpart('alt:'+k,path)
    if type(a)==type(v) and a==v and ((av is not MISSING and av!=v and av==alt) or (av is MISSING and request(ps.get('alt:'+k,{})) is not None and isinstance(v,bool))):
     node=arg_node(k,path)
     if av is MISSING:node['omit_false']=True
     break
  return wrap(node,path)
 tree=walk(b,[])
 # JSON:API map query knobs use arbitrary user keys, not fixture keys.
 query=tree['fields']['query']
 if query.get('op')=='object':
  for k in ('filter','fields'):
   if k in properties:
    for key in list(query['fields']):
     if key==k+'[migration]':del query['fields'][key]
    used.add(k)
 return {'request':tree,'used':sorted(used),'args':args}

out={}; unbound={}
for tool in TOOLS:
 name=tool['name'];ps=PROBES[name];p=dict(tool['inputSchema'].get('properties',{}));p.update({k:v for k,v in ACTUAL.get(name,{}).items() if k not in p});base=ps['full']
 entry=compile_recipe(name,base,ps,p);entry['kind']='compound' if name in COMPOUND else 'request';entry['supports_cursor']=bool(request(ps.get('cursor',{})) and request(ps['cursor'])['path']=='/v1/apps' and request(ps['cursor'])['query'].get('cursor')==['migration']);entry['query_maps']=[k for k in ('filter','fields') if k in p and k+'[migration]' in request(base)['query']]
 entry['query_map_overrides']=[]
 for mapname in entry['query_maps']:
  collision=request(ps.get('collision:'+mapname,{}))
  if collision:
   for key,values in collision['query'].items():
    if key.startswith(mapname+'[') and values==['COLLISION_'+key[len(mapname)+1:-1]]:entry['query_map_overrides'].append(key)
 branches=[]
 for k in p:
  if k not in base['args'] and k not in ('cursor','file_path'):
   candidate=ps.get('alt:'+k,{})
   if request(candidate):branches.append({'when':k,**compile_recipe(name,candidate,ps,p)})
 if name=='list_app_store_version_experiments':
  candidate=ps['omit:version_id'];branches.append({'when_absent':'version_id',**compile_recipe(name,candidate,ps,p)});None
 for label,candidate in ps.items():
  if label.startswith('enum:') and request(candidate):
   _,key,value=label.split(':',2)
   if request(candidate)['path']!=request(base)['path']:branches.append({'when':key,'equals':value,**compile_recipe(name,candidate,ps,p)})
 if branches:entry['branches']=branches
 used=set(entry['used'])|{x for branch in branches for x in branch['used']}
 unknown=set(p)-used-{'cursor','file_path','file_data_base64','limit'}
 if name in COMPOUND:unknown=set()
 recognized_runtime={'list_in_app_purchase_price_schedule_prices':{'price_type'},'submit_app_for_review':{'version_id'},'get_background_asset_version_release':{'release_type'}}
 unknown-=recognized_runtime.get(name,set())
 unknown-=set(tool['inputSchema'].get('properties',{}))-set(ACTUAL.get(name,tool['inputSchema'].get('properties',{})))
 if name=='update_age_rating_declaration':unknown.discard('horror_or_fear_themes')
 if unknown:unbound[name]=sorted(unknown)
 entry['legacy_ignored']=sorted(set(tool['inputSchema'].get('properties',{}))-set(ACTUAL[name])) if name in ACTUAL else []
 if name=='update_age_rating_declaration':entry['legacy_ignored'].append('horror_or_fear_themes')
 out[name]=entry
Path('data/request_recipes.json').write_text(json.dumps(out,indent=2)+'\n')
print('recipes:',len(out),'compound:',len(COMPOUND),'unclassified parameters:',json.dumps(unbound,indent=2))
