/**
 * i18n shard: calibrationEdit (en-US) — parallel-development shard.
 *
 * Rules (enforced by scripts/i18n-check.mjs):
 * - only keys new to this area; never redefine keys already in src/i18n/en-US.ts;
 * - zh-CN / en-US shards must have identical key sets (committed together);
 * - this area owns the top-level namespace calibrationEdit.*.
 */

const messages = {
  calibrationEdit: {
    list: {
      notAvailableTitle: 'Calibration backend not available (NOT_AVAILABLE)',
      notAvailableReason: 'The daemon reports this capability as unavailable. Reason: {reason}',
      notAvailableNoReason: 'The daemon reported no reason (empty reasons): no fake jobs and no fabricated progress are shown.',
      engineUnknown: 'not reported',
      engineUnknownReason: 'The daemon did not report an engine field: engine truthfulness is unknown, so it is not labelled measured.',
      reasonLabel: 'Reason'
    },
    staged: {
      title: 'Two-phase draft (prepare)',
      desc: 'prepare only creates a draft and returns a one-shot engine_token; the running state is unchanged. It takes effect only after commit, or the draft can be dropped with abort.',
      prepareAction: 'Two-phase prepare',
      prepareHint: 'Creates a draft only; the running state does not change until commit.',
      draftOnly: 'draft, not applied',
      token: 'engine_token',
      planSha: 'artifact plan sha256',
      previousVersion: 'version before apply',
      targetVersion: 'rollback target version',
      commit: 'Commit',
      committing: 'Committing…',
      abort: 'Discard draft (abort)',
      aborting: 'Discarding…',
      commitDone: 'Two-phase commit applied (server receipt is authoritative).',
      abortDone: 'Draft discarded; running state unchanged.',
      pendingTitle: 'Uncommitted two-phase draft',
      pendingDesc: 'The token can be consumed once; the running plan stays unchanged until commit applies or abort discards it.',
      noPending: 'No pending prepare draft: use "Two-phase prepare" first.',
      notAppliedHint: 'This is a prepare draft: it is NOT applied and must not be shown as applied/rolled back.',
      tokenMissingJob: 'The prepare receipt has no job_id, so it cannot be committed/discarded per job (use the single-shot apply or prepare again).'
    },
    wizardApply: {
      jobTitle: 'Wizard apply receipt',
      jobDesc: 'The server returned job_id / location (a config draft or a created job); effectiveness is decided by the job detail and control-plane receipt.',
      jobId: 'job id',
      location: 'location'
    },
    receipt: {
      applied: 'Apply receipt (applied)',
      rolledBack: 'Rollback receipt (rolled back)',
      prepared: 'Prepare draft receipt (not applied)',
      aborted: 'Abort receipt (draft discarded)',
      appliedVersion: 'applied version',
      previousVersion: 'previous version',
      restoredVersion: 'restored version',
      graphId: 'graph_id',
      sha256: 'sha256',
      snapshot: 'snapshot before apply',
      ack: 'receipt type',
      ackActual: 'actual (really applied)',
      ackSynthetic: 'synthetic (simulated commit)',
      appliedAt: 'applied at',
      rolledBackAt: 'rolled back at',
      jobId: 'job id',
      planSha: 'artifact plan sha256',
      targetVersion: 'rollback target version',
      control: 'control-plane receipt',
      abortedDesc: 'Running state unchanged (server receipt is authoritative).',
      prepareDesc: 'Draft only; running state unchanged.'
    },
    artifact: {
      download: 'Download (authenticated)',
      downloading: 'Downloading…',
      failed: 'Artifact download failed',
      done: 'Artifact downloaded: {filename}',
      filenameFallback: 'Server sent no Content-Disposition filename; using the artifact name: {name}',
      shaHeader: 'artifact sha256 reported by the daemon (not recomputed locally)'
    },
    error: {
      noPendingDraft: 'There is no pending prepare draft locally (no commit/abort request was sent).'
    }
  }
}

export default messages
