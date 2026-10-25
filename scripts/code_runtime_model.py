"""Executable semantic reference model; NOT a production runtime or backend test."""
from dataclasses import dataclass, field

class ContractError(Exception):
    def __init__(self,code): super().__init__(code); self.code=code

@dataclass
class Job:
    owner: str
    project: str
    execution: str='queued'
    commit: str='not_started'
    projection: str='not_required'
    events: list=field(default_factory=list)
    receipt: str|None=None

class Model:
    def __init__(self):
        self.jobs={};self.receipts={};self.events={};self.etags={};self.projections={};self.revisions={}
    def authorize(self,permissions,required,principal_project,resource_project,visible=True):
        if not visible: raise ContractError('NOT_FOUND')
        if principal_project != resource_project: raise ContractError('NOT_FOUND')
        if required not in permissions: raise ContractError('FORBIDDEN')
    def accept(self,job_id,key,owner,project):
        if key in self.receipts: return self.jobs[self.receipts[key]]
        if job_id in self.jobs: raise ContractError('ETAG_MISMATCH')
        job=Job(owner,project);self.jobs[job_id]=job;self.receipts[key]=job_id
        self.publish('code.analysis.accepted',job_id,{'operation_id':key,'job_id':job_id,'repository_ref':project,'source_revision':'sha'})
        return job
    def publish(self,kind,aggregate,payload):
        event_key=(kind,aggregate,payload.get('operation_id'))
        if event_key in self.events: return False
        self.events[event_key]=payload;return True
    def cancel(self,job_id,project,owner):
        job=self.jobs.get(job_id)
        if job is None or job.project!=project or job.owner!=owner:raise ContractError('JOB_NOT_FOUND')
        if job.commit in ('committed','unknown') or job.execution in ('completed','cancelled','failed'):
            raise ContractError('JOB_NOT_CANCELLABLE')
        job.execution='cancelled';job.commit='not_committed';job.projection='not_required'
        self.publish('code.analysis.cancelled',job_id,{'operation_id':job_id,'job_id':job_id,'repository_ref':project,'source_revision':'sha'})
        return job
    def commit(self,job_id,receipt):
        job=self.jobs[job_id]
        if job.execution not in ('queued','running') or job.commit!='not_started':raise ContractError('JOB_NOT_CANCELLABLE')
        job.execution='running';job.commit='committed';job.projection='pending';job.receipt=receipt
        return job
    def finish_projection(self,job_id,success):
        job=self.jobs[job_id]
        if job.commit!='committed':raise ContractError('STORAGE_COMMIT_UNKNOWN')
        job.execution='completed';job.projection='ready' if success else 'failed';return job
    def require_generation(self,repository,generation):
        if self.projections.get(repository,0)<generation:raise ContractError('INDEX_NOT_READY')
    def invalidate(self,revision,etag,key):
        if key in self.receipts:return self.receipts[key]
        old=self.etags.get(revision,'0')
        if old!=etag:raise ContractError('ETAG_MISMATCH')
        new=str(int(old)+1);self.etags[revision]=new;self.receipts[key]=new
        self.publish('code.revision.stale',revision,{'operation_id':key,'repository_ref':'repo','source_revision':revision,'canonical_revision':new})
        return new
    def filter_edges(self,edges,visible_nodes,visible_edges):
        return [e for e in edges if e['src'] in visible_nodes and e['dst'] in visible_nodes and e['id'] in visible_edges]
