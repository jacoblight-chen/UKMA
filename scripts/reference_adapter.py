"""Python executable contract reference; independent of uncompiled Rust adapter."""
import json,os,tempfile,threading
class Error(Exception): pass
class Adapter:
    def __init__(self,path):
        self.path=path;self.lock=threading.RLock()
        self.state=json.load(open(path)) if os.path.exists(path) else {'symbols':{},'keys':{},'events':[],'next':0}
    def _commit(self,s):
        fd,tmp=tempfile.mkstemp(dir=os.path.dirname(self.path) or '.')
        try:
            with os.fdopen(fd,'w') as f:json.dump(s,f);f.flush();os.fsync(f.fileno())
            os.replace(tmp,self.path)
        finally:
            if os.path.exists(tmp):os.unlink(tmp)
        self.state=s
    def put(self,scope,item,key,etag=None):
        with self.lock:
            if not key:raise Error('KEY_REQUIRED')
            if (scope['tenant'],scope['project'])!=(item['tenant'],item['project']):raise Error('DENIED')
            s=json.loads(json.dumps(self.state)); idem=f"{scope['tenant']}:{scope['project']}:{key}"
            fingerprint=json.dumps(item,sort_keys=True)
            if idem in s['keys']:
                prev=s['keys'][idem]
                if prev['fingerprint'] != fingerprint:raise Error('IDEMPOTENCY_CONFLICT')
                return prev['result']
            old=s['symbols'].get(item['id'])
            if old and (old['tenant'],old['project']) != (scope['tenant'],scope['project']):raise Error('DENIED')
            if (old and etag!=old['etag']) or (not old and etag is not None):raise Error('ETAG_CONFLICT')
            out=dict(item,etag=(old['etag']+1 if old else 1));s['symbols'][item['id']]=out
            s['keys'][idem]={'fingerprint':fingerprint,'result':out}
            s['next']+=1;s['events'].append({'id':s['next'],'type':'code.symbol.upserted','aggregate_id':item['id'],'delivered':False})
            self._commit(s);return out
    def get(self,scope,id):
        s=self.state['symbols'].get(id)
        if not s or (s['tenant'],s['project'])!=(scope['tenant'],scope['project']):raise Error('NOT_FOUND')
        return s
    def pending(self):return [x for x in self.state['events'] if not x['delivered']]
    def ack(self,id):
        with self.lock:
            s=json.loads(json.dumps(self.state))
            matches=[x for x in s['events'] if x['id']==id]
            if not matches:raise Error('NOT_FOUND')
            matches[0]['delivered']=True;self._commit(s)
