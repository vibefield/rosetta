import json, sys
N='num?'; NN='num'; B='bool'; BN='bool?'; H='sha256'; HN='sha256?'
def E(*vals, null=False): return {'enum': list(vals), **({'null': True} if null else {})}
def R(name): return {'ref': name}
def O(d, opt=None, null=False):
    o = {'obj': d}
    if opt: o['opt'] = opt
    if null: o['null'] = True
    return o
def A(item, mx=100000, null=False): return {'arr': item, 'max': mx, **({'null': True} if null else {})}
def T(*items): return {'tuple': list(items)}
def M(val, keys, null=False): return {'map': val, 'keys': keys, **({'null': True} if null else {})}
def ID(form, null=False): return {'id': form, **({'null': True} if null else {})}

AGENTS = ['antigravity', 'claude-code', 'codex', 'copilot', 'grok', 'hermes', 'kimi-code', 'opencode', 'pi']
PHASES = ['discovering', 'awaiting_consent', 'backfilling', 'metadata_ready', 'restoring', 'finalizing', 'publishing', 'live', 'none', 'other']
SEARCH = ['ready', 'building', 'catching-up', 'stale', 'failed', 'none', 'other']
RUNSTATE = ['running', 'pausing', 'paused', 'succeeded', 'failed', 'none', 'other']
AGENTS_SEEN = AGENTS + ['other']
REACHED = E('yes', 'not-reached', 'not-applicable')
def REACH(*keys): return O({k: REACHED for k in keys})
OUTCOME_REASONS = ['app-fatal', 'app-exited', 'daemon-exited', 'script-error', 'daemon-did-not-stop', 'dpr-mismatch', 'script-step-timeout']
FACES = ['connecting', 'not-answering', 'consent', 'cold-start', 'roll-call-looking', 'roll-call-ready', 'roll-call-starting', 'cols', 'none', 'other']
STATES = ['disconnected', 'complete', 'discovering', 'metadata', 'restoring', 'reading', 'organizing', 'finalizing', 'publishing', 'paused', 'pausing', 'failed',
          'search_failed', 'search_waiting', 'search_indexing', 'search_optimizing', 'search_validating', 'search_publishing', 'none', 'other']
LABELS = ['error', 'team', 'seeking', 'preparing', 'loading', 'other']
AIMS = ['begin', 'kind', 'newest-project', 'other-project', 'newest-row', 'recent-next', 'ready-next', 'pending-row']
EVENTS = ['pointerdown', 'pointerup', 'click', 'keydown', 'keyup', 'keypress', 'input', 'mousedown', 'mouseup']
INPUTS = ['pointerdown', 'pointerup', 'wheel', 'keydown']
MARKS = ['enrolled-ok', 'enrolled-mismatch', 'consent-begin', 'select-newest-project', 'select-newest-project-missed', 'open-newest', 'open-pending', 'open-recent', 'aim-horizon', 'reselect',
         'open-finalizing', 'open-post-canonical', 'search-open', 'search-query', 'search-close', 'park', 'done',
         'newest-never-ready', 'horizon-via-all-readable',
         'timeout-cols', 'timeout-frozen', 'timeout-newest-ready', 'timeout-open', 'timeout-horizon', 'timeout-all-readable', 'timeout-canonical',
         'timeout-search', 'timeout-sheet', 'timeout-search-ready', 'timeout-charts'] + [f'absent:{k}' for k in AIMS + ['park']]
MILESTONES = ['consent-granted', 'metadata-ready', 'first-session-complete', 'first-readable', 'recent-ready:24h', 'recent-ready:7d', 'recent-ready:24h@newest',
  'recent-ready:7d@newest', 'newest-trees-ready', 'newest-giant-ready', 'small-recent-ready:24h@newest', 'small-recent-ready:7d@newest',
  'runtime-metadata-public:claude-code', 'runtime-metadata-public:codex', 'runtime-readable:claude-code', 'runtime-readable:codex',
  'canonical-ready', 'memory-peak', 'search-ready', 'search-failed']
HARNESS = ['run_ui.mjs', 'app-entry.mjs', 'reader-context.mjs', 'probe.js', 'probe-worker.js', 'drain.mjs', 'daemon-lines.mjs', 'metrics.mjs', 'privacy.mjs',
  'digests.mjs', 'source.mjs', 'fixture.mjs', 'ui-run-1.schema.json', 'scripts/reader-v1.mjs', 'scripts/passive-v1.mjs'] + ['r1-bench/' + f for f in
  ['verify_r1_snapshot.js', 'r1_snapshot_lib.js', 'r1_logical_digest.sql', 'r1_cursor_order.sql', 'r1_ranked.py', 'product-diagnostic-v3.json']]
DEVIATIONS = ['background-throttling-off', 'worktree-ipc-denied', 'favicon-ipc-denied', 'browser-ipc-denied', 'external-links-denied',
  'network-local-only', 'announce-dir-private', 'fresh-profile', 'about-blank-before-load', 'focus-emulated', 'device-scale-factor-forced']
STUBBED = ['ah-worktree-open', 'ah-worktree-git-facts', 'ah-worktree-file-action', 'ah-favicon', 'ah-file-browsers', 'ah-file-open-browser-preview',
  'shell-open-external', 'network-blocked', 'foreign-binding-calls']
MSGTYPES = ['agent', 'collaboration', 'notice', 'summary', 'thinking', 'tool_use', 'user', 'tool_result', 'reasoning', 'plan', 'task', 'artifact']
COUNTS = ['sessions', 'projects', 'messages', 'tool_rows', 'usage_bearing_messages', 'session_usage_rows', 'session_usage_total_rows', 'usage_bucket_rows',
  'file_edit_rows', 'git_commit_rows', 'plan_rows', 'task_rows', 'workflow_run_rows', 'workflow_journal_rows', 'html_artifact_rows', 'drift_agg_rows',
  'drift_seen_rows', 'ingest_cursor_rows', 'ingest_cursor_active', 'ingest_cursor_sqlite_members_active', 'agent_runtime_rows',
  'rows_with_asset_refs:message.detail', 'rows_with_asset_refs:message.tool_detail', 'rows_with_asset_refs:html_artifact.asset'] + \
  [f'message_type:{t}' for t in MSGTYPES] + [f'session_stream_state:{s}' for s in ['pending', 'streaming', 'indexed']] + [f'session_kind:{k}' for k in AGENTS]
ROLES = {'claim': E('claim-candidate'), 'guard': E('guard'), 'invariant': E('invariant'), 'report': E('report-only')}

defs = {
  'pair': T(N, N), 'triple': T(N, N, N),
  'dist': O({'n': NN, 'p50Ms': N, 'p95Ms': N, 'maxMs': N}),
  'window': O({'wallMs': N, 'longestMs': N, 'blockingMs': N, 'blockingPct': N, 'n100': N, 'nLoaf': N}),
  'phaseStat': O({'n': NN, 'p50Ms': N, 'maxMs': N}),
  'horizon': O({'receiptMs': N, 'paintedSelMs': N, 'roots': N, 'rootsInSelected': N, 'neverRosteredExcluded': N, 'viaCaptureOver': B,
                 'daemonHorizonSessions': N, 'daemonMs': N, 'lagVsDaemonMs': N}),
  'check': O({'violations': NN, 'exercised': NN, 'pass': B}),
  'listMoves': O({'moves': NN, 'px': NN, 'movesDiscrete': NN, 'pxDiscrete': NN}),
  'twoLists': O({'projects': N, 'sessions': N}),
}

metrics = O({
  'H1': O({'role': ROLES['guard'], 'ms': N, 'projects': N, 'metaPushMs': N, 'statusPollMs': N, 'lagVsMetadataReadyMs': N, 'reached': REACH('ms')}),
  'H2': O({'role': ROLES['claim'], 'ms': N, 'receiptMs': N, 'heldReadyMs': N, 'firstDrawMs': N, 'firstDrawStanding': E('ready', 'not-imported', 'none'),
           'indexAtFirstDraw': N, 'flippedBeforeDrawn': BN, 'newestExcluded': B, 'lag': O({'role': ROLES['guard'], 'ms': N, 'rawMs': N}),
           'reached': REACH('ms', 'receiptMs')}),
  'H3': O({'role': ROLES['claim'], 'h24': R('horizon'), 'h7d': R('horizon'),
           'reached': REACH('h24.receiptMs', 'h24.paintedSelMs', 'h7d.receiptMs', 'h7d.paintedSelMs')}),
  'Hall': O({'role': ROLES['claim'], 'receiptMs': N, 'paintedSelMs': N, 'daemonMs': N, 'lagVsDaemonMs': N, 'neverRostered': N, 'metadataOnlyRoots': N,
             'reached': REACH('receiptMs', 'paintedSelMs')}),
  'H4': O({'role': ROLES['guard'], 'n': NN, 'unresolved': NN, 'p50Ms': N, 'maxMs': N,
           'byPhase': O({'reading': R('phaseStat'), 'finalizing': R('phaseStat'), 'postCanonical': R('phaseStat')}),
           'pendingFace': O({'role': ROLES['guard'], 'n': NN, 'p50Ms': N, 'maxMs': N}), 'reached': REACH('samples', 'pendingFace')}),
  'H5': O({'role': ROLES['guard'], 'n': NN, 'p50Ms': N, 'p95Ms': N, 'maxMs': N, 'reached': REACH('samples')}),
  'H6': O({'role': ROLES['guard'], 'loafThresholdMs': NN, 'byWindow': O({'streaming': R('window'), 'finalizing': R('window'), 'post': R('window')}),
           'input': O({'role': ROLES['guard'], 'scripted': NN, 'reported': NN, 'p98Ms': N, 'maxMs': N, 'unreportedAsMs': NN}),
           'report': O({'role': ROLES['report'], 'heapPeakMb': N, 'appWorkingSetPeakMb': N, 'probeCostKind': E('js-self-time'), 'probeMainMs': N,
                        'probePostMs': N, 'probePostMb': N, 'probePasses': N, 'probeErrors': N, 'probeMainPctOfWall': N, 'probeBudgetPct': NN,
                        'probeWithinBudget': BN, 'workerParseMs': N}),
           'reached': REACH('longestFrame', 'input')}),
  'H7': O({'role': ROLES['guard'], 'projectsMoves': N, 'projectsPx': N, 'sessionsMoves': N, 'sessionsPx': N,
           'discreteInput': O({'role': ROLES['report'], 'projectsMoves': N, 'projectsPx': N, 'sessionsMoves': N, 'sessionsPx': N, 'selectionJumps': N}),
           'heldMs': R('twoLists'), 'pillShown': R('twoLists'),
           'layoutShift': N, 'layoutShiftNoInput': N, 'invariant': O({'role': ROLES['invariant'], 'pass': B, 'selectionJumps': N, 'aims': NN, 'aimMisses': NN,
                         'missedOtherRow': NN, 'missedNoRow': NN, 'missedTargetShiftPx': A(N, 1000)}),
           'reached': REACH('moves', 'aims')}),
  'H8': O({'role': ROLES['guard'], 'chartsSelectedMs': N, 'chartsVisibleMs': N, 'chartsAllMs': N, 'portfolioMs': N, 'chartsSelectedLagMs': N, 'chartsAllLagMs': N,
           'searchLagMs': N, 'queries': NN, 'typedToHitMs': A(N, 16), 'rows': A(N, 16), 'hitDigest': HN,
           'report': O({'role': ROLES['report'], 'searchHitMs': N, 'searchAfterDaemonReadyMs': N, 'typedAfterUiReadyMs': N}),
           'reached': REACH('chartsSelectedMs', 'chartsVisibleMs', 'chartsAllMs', 'portfolioMs', 'searchLagMs')}),
  'I1': O({'role': ROLES['invariant'], 'pass': B, 'checks': O({
      'c1ReadyBeforeReceipt': O({'violations': NN, 'exercised': NN, 'checkedRows': NN, 'pass': B}),
      'c2NeverReverts': R('check'),
      'c3SelectionOnlyByInput': O({'violations': NN, 'exercised': NN, 'selectionChanges': NN, 'violationsDiscreteInput': NN, 'pass': B}),
      'c4NoFlashOfEmpty': O({'violations': NN, 'exercised': NN, 'emptyShown': NN, 'indexingShown': NN, 'pass': B}),
      'c5EndState': O({'violations': NN, 'exercised': NN, 'pass': B, 'pendingRoots': N, 'pendingSessions': N, 'metadataOnlyRoots': N, 'neverRostered': N,
                       'daemonNeverRosteredSessions': N, 'daemonMetadataSessions': N, 'rosterSessions': N, 'daemonPendingRoots': N,
                       'projectMismatches': N, 'newestProjectRoots': N, 'newestProjectDaemonRoots': N, 'renderedNewestProjectRows': N,
                       'readyCountDone': N, 'readyCountTotal': N, 'uiReadySessions': N, 'daemonReadableSessions': N, 'membershipStable': BN,
                       'enrolledExact': B, 'daemonListTruncated': BN}),
      'c6Privacy': R('check'), 'c7PendingFaceCourse': R('check')}, opt=['c6Privacy'])}),
  'D1': O({'role': ROLES['report'], 'consentToProgressMs': N, 'beginToConsentGrantedMs': N,
           'statusRtt': O({'n': NN, 'p50Ms': N, 'p99Ms': N, 'aborted': N, 'maxGapMs': N}), 'readyCountLag': R('dist'),
           'countUnitRatio': O({'at25': N, 'at50': N, 'at100': N}), 'phases': A(T(N, E(*STATES)), 200),
           'phaseSeenLagMs': O({'finalizing': N, 'canonical': N, 'search': N}), 'titleRowsAfterSearchMs': N,
           'appLaunch': O({'windowMs': N, 'loadedMs': N, 'firstFrameMs': N, 'rollCallMs': N, 'rollCallReadyMs': N})}),
  'D2': O({'role': ROLES['report'], 'newestProjectRank': A(T(NN, N, N, N), 8), 'inversionSelectedMs': N, 'inversionCorpusMs': N,
           'newestGiant': O({'known': B, 'receiptMs': N, 'paintMs': N, 'daemonMs': N, 'lagVsDaemonMs': N}), 'notImportedSelected': A(R('pair'))}),
  'D3': O({'role': ROLES['report'], 'opens': A(O({'standing': E('ready', 'not-imported', 'unavailable'), 'phase': E('reading', 'finalizing', 'postCanonical'),
           'clickToSocketMs': N, 'socketToOpenMs': N, 'openToHelloMs': N, 'helloToSnapshotMs': N, 'snapshotBytes': N, 'parseMs': N, 'snapshotToRevealMs': N,
           'revealToContentMs': N, 'coverMs': N, 'coverLabel': E(*LABELS, 'none'), 'readerWaitMs': N}), 64),
           'covers': N, 'coversOver100': N, 'coverLabels': M(NN, LABELS), 'recovers': N, 'errorCovers': N, 'sessionSockets': N, 'secondSockets': N,
           'sessionResyncs': N, 'wsErrorFrames': N, 'http503': N, 'httpNotOk': N, 'longestSnapshotWaitMs': N,
           'meta': O({'frames': N, 'snapshots': N, 'mutations': N, 'resyncs': N, 'bytes': N, 'flipFrames': N, 'sockets': N}),
           'wire': R('dist'), 'uiLoadTax': N}),
}, null=True)

probe_end = O({k: N for k in ['rootsSeen', 'readyAll', 'sessionsSeen', 'pendingSessions', 'pendingRoots', 'metaOnlyRoots', 'neverRostered', 'lastRootReady', 'daemonRoots',
  'daemonPending', 'projectMismatches', 'newestProjectRoots', 'newestProjectDaemonRoots', 'inversionMs', 'workerParseMs', 'frames', 'snapshots',
  'mutations', 'resyncs', 'metaBytes', 'statusBodies', 'retractions']} | {'daemonTruncated': BN, 'membershipStable': BN}, null=True)
probe = O({
  'format': E('ui-probe/1'),
  'selfTest': O({'firstMetaAt': N, 'wsWrapped': B, 'fetchWrapped': B, 'worker': E('none', 'worker', 'failed'), 'loaf': B, 'eventTiming': B, 'layoutShift': B}),
  'page': O({'firstFrameAt': N, 'firstRollCallAt': N, 'rollCallReadyAt': N, 'dpr': N, 'innerW': N, 'innerH': N, 'focusedAtStart': B, 'focusedAtRollCall': BN}),
  'faces': A(T(NN, E(*FACES)), 1000), 'states': A(T(NN, E(*STATES)), 1000), 'readyCount': A(R('pair')), 'readyRoots': A(R('triple')),
  'notImported': A(R('pair')), 'marks': A(T(NN, E(*MARKS)), 1000), 'inputs': A(T(NN, E(*INPUTS)), 100000),
  'status': O({'rtt': A(R('pair')), 'aborted': NN, 'firstNonConsentAt': N, 'firstCanonicalAt': N, 'firstSearchReadyAt': N, 'firstLiveAt': N,
               'metaReadyAtWall': N, 'firstPostConsentPhase': E(*PHASES, null=True),
               'last': O({'phase': E(*PHASES), 'canonical': B, 'search': E(*SEARCH), 'done': NN, 'total': NN, 'run': E(*RUNSTATE)}, null=True),
               'kinds': A(E(*AGENTS_SEEN), 16), 'ingestKinds': A(E(*AGENTS_SEEN), 16)}),
  'meta': O({'sockets': NN, 'firstProjectFrameAt': N, 'flipFrames': A(NN), 'selFlips': A(NN), 'retractions': NN}),
  'frozen': O({'at': NN, 'roots': NN, 'metaOnly': NN, 'newestKnown': B, 'projectRoots': NN}, null=True),
  'h1': O({'paintAt': NN, 'projects': NN}, null=True),
  'h2': O({'firstDrawAt': N, 'firstDrawStanding': E('ready', 'not-imported', null=True), 'indexAtFirstDraw': N, 'paintReadyAt': N, 'receiptAt': N, 'heldReadyMs': N,
           'newestExcluded': B}),
  'h3': O({k: N for k in ['h24ReceiptAt', 'h7dReceiptAt', 'n24', 'n7d', 'n24Sel', 'n7dSel', 'h24PaintSelAt', 'h7dPaintSelAt', 'excluded24', 'excluded7d', 'excludedSel']}
          | {'h24ViaCaptureOver': B, 'h7dViaCaptureOver': B}),
  'hall': O({'paintSelAt': N}),
  'opens': A(O({'clickAt': NN, 'standing': E('ready', 'not-imported', 'unavailable'), 'phase': E(*PHASES), 'canonical': B, 'contentAt': N, 'faceAt': N,
                'course': E('none', 'newest-first', 'runtime-close', 'resumed', 'stopped', 'other'), 'coverAt': N, 'coverEndAt': N, 'label': E(*LABELS, 'none'), 'revealAt': N,
                'wsAt': N, 'openAt': N, 'helloAt': N, 'snapAt': N, 'snapBytes': N, 'parseMs': N}), 64),
  'h5': A(R('pair')), 'loaf': A(R('triple')), 'events': A(T(NN, NN, E(*EVENTS), NN, NN)),
  'layoutShift': O({'sum': NN, 'sumNoInput': NN, 'n': NN}),
  'h7': O({'projects': R('listMoves'), 'sessions': R('listMoves'), 'selectionJumps': NN, 'selectionJumpsDiscrete': NN, 'aims': A(T(NN, E(*AIMS), NN, N, E('aimed', 'other-row', 'none')), 1000),
           'heldMs': R('twoLists'), 'pillShown': R('twoLists')}),
  'h8': O({'chartsSelectedAt': N, 'chartsVisibleAt': N, 'chartsAllAt': N, 'portfolioAt': N,
           'search': A(O({'q': NN, 'beginAt': NN, 'typedAt': N, 'firstFulltextAt': N, 'firstTitleAt': N, 'quietAt': N, 'rows': N, 'fulltextRows': N, 'digest': HN}), 16)}),
  'i1': O({'c1Before': NN, 'c1PendingReady': NN, 'c2': NN, 'c3': NN, 'c3Discrete': NN, 'c3ByList': R('twoLists'), 'c3Frames': NN, 'selectionChanges': NN,
           'c4': NN, 'c4Shown': NN, 'c4Indexing': NN, 'c4Opens': NN, 'c7': NN, 'c7Faces': NN, 'checkedRows': NN}),
  'd1': O({'beginAt': N, 'progressAt': N, 'titleRowsAfterSearchMs': NN, 'sheetAfterSearch': B}),
  'd2': O({'rank': A(T(NN, N, N, N), 8), 'inversionSelMs': NN, 'selShown': B, 'giantReceiptAt': N, 'giantPaintAt': N, 'giantKnown': B}),
  'd3': O({'covers': NN, 'coversOver100': NN, 'labels': M(NN, LABELS), 'recovers': NN, 'errorCovers': NN, 'sockets': NN, 'secondSockets': NN,
           'sessionResyncs': NN, 'wsErrorFrames': NN, 'http503': NN, 'httpNotOk': NN, 'longestSnapshotWaitMs': N}),
  'renderedNewestProjectRows': N,
  'cost': O({k: NN for k in ['mainMs', 'postMs', 'postBytes', 'posts', 'passes', 'passMs', 'apiMs', 'observerCalls', 'errors']}),
  'end': probe_end, 'finishedAt': NN,
}, null=True)

root = O({
  'format': E('ui-run/1'),
  'script': O({'name': E('reader-v1', 'passive-v1'), 'sha256': H}),
  'identity': O({'rosettadSha256': H, 'distSha256': H, 'distFiles': NN, 'electron': ID('version'),
    # Exactly one of snapshot (--snapshot, the frozen R1 corpus) and fixture (--home, a
    # synthetic fixture home from fixture.mjs) is set; the other is null. The schema
    # forms cannot say "exactly one", so privacy.mjs validateRun enforces it.
    'snapshot': O({'walkInputsDigest': H, 'manifestSha256': H, 'files': NN, 'bytes': NN}, null=True),
    'fixture': O({'name': E('ui-fixture-v1'), 'specSha256': H, 'treeSha256': H, 'files': NN, 'bytes': NN, 'walkInputsDigest': H,
      'generatorSha256': H, 'baseEpochMs': NN, 'giantAgeH': NN, 'agesH': A(NN, 64), 'stride': NN,
      'corpora': A(O({'agent': E(*AGENTS), 'sessions': NN, 'lines': NN, 'seed': NN, 'toolRatio': NN, 'giant': B}), 8),
      'verifyWallMs': NN, 'idleGuard': B}, null=True),
    'harnessSha256': M(H, HARNESS), 'machine': O({'model': ID('machine-model'), 'os': ID('os-version'), 'cpus': NN, 'memGb': NN}),
    'windowPx': ID('window'), 'dpr': N, 'dprMeasured': N, 'deviations': A(E(*DEVIATIONS), 16), 'enrolled': A(E(*AGENTS), 16)}),
  'run': O({'startedAt': ID('iso-time'), 'outcome': E('complete', 'incomplete', 'timeout', 'failed'), 'outcomeReasons': A(E(*OUTCOME_REASONS), 16), 'wallS': NN,
    'daemonExit': O({'code': N, 'signal': E('SIGINT', 'SIGKILL', 'SIGTERM', null=True)}), 'electronExit': N,
    'guard': O({'cpuIdleAtStart': NN, 'waitedS': NN, 'proceededOverGuard': B, 'load1': NN, 'idleMin': NN, 'swapMbBefore': N, 'swapMbAfter': N, 'freeGbBefore': NN}),
    'snapshotVerify': O({'ok': B, 'files': NN, 'bytes': NN, 'wallMs': NN}, null=True), 'scriptOk': B, 'scriptTimeouts': NN, 'selfTestPass': B, 'invariantsPass': B,
    'stubbed': M(NN, STUBBED, null=True),
    'window': O({'contentW': NN, 'contentH': NN, 'dpr': NN, 'displayW': NN, 'displayH': NN}, null=True),
    'page': O({'dpr': N, 'innerW': N, 'innerH': N, 'focusedAtStart': B, 'focusedAtRollCall': BN}, null=True),
    'inputs': M(NN, ['move', 'down', 'up', 'wheel', 'key', 'key-up', 'shortcut']), 'marks': A(T(N, E(*MARKS)), 1000),
    'newestGiantKnown': B, 'newestGiantTreeBytes': N}),
  'clocks': O({'method': E('clock-line', 'metadata-ready-anchor', 'none'), 't0Kind': E('consent-granted', 'begin-click', 'none'),
    'rendererOffsetMs': N, 'rendererRttMs': N, 'rendererOffsetApplied': B, 'mainOffsetMs': N, 'mainRttMs': N, 'mainOffsetApplied': B,
    'rendererDriftMs': N, 'mainDriftMs': N, 'roundedMilestones': NN, 'daemonReceiptLagMs': O({'min': N, 'p50': N, 'n': NN}), 'estimatedErrorMs': N}, null=True),
  'selfTest': O({'firstMeta': B, 'wsWrapped': B, 'fetchWrapped': B, 'worker': E('none', 'worker', 'failed'), 'loaf': B, 'eventTiming': B, 'layoutShift': B, 'pass': B}, null=True),
  'daemon': O({'milestonesS': M(N, MILESTONES), 'milestonesMs': M(N, MILESTONES, null=True), 'horizonSessions': M(N, MILESTONES), 'newestGiantTreeBytes': N,
    'treeAdmission': O({'batches': NN, 'roots': NN, 'sessions': NN, 'maxLockHeldMs': NN}),
    'memoryPeak': O({'footprintMb': N, 'residentMb': N, 'swapUsedMb': N, 'swapouts': N, 'pageouts': N}, null=True),
    'runtimeReadableSessions': NN, 'metadataPublicSessions': N, 'unknownMilestones': NN, 'stdoutLines': NN, 'jsonEvents': NN, 'panicLines': NN, 'stderrLines': NN, 'stderrLockedLines': NN}),
  'metrics': metrics,
  'equality': O({'logicalTotal': HN, 'strictTotal': HN, 'strictNoidTotal': HN, 'strictNoidRTotal': HN, 'surrogateTotal': HN, 'logicalOutSha256': HN,
    'cursorOrderOutSha256': HN, 'counts': M(NN, COUNTS, null=True), 'sourceSetDigest': HN, 'sourceDigest': HN, 'metaDigest': HN, 'entryDigest': HN,
    'ledgerVerifyOk': B, 'rankedIdsSha256': HN, 'rankedSha256': HN, 'rankedWarmExit': N, 'hitDigest': HN, 'finalRoots': N, 'finalReadySessions': N}),
  'privacy': O({'schema': E('pending', 'pass', 'fail'), 'forbiddenScan': E('pending', 'pass', 'fail'), 'forbiddenValues': NN,
                'forbiddenSource': E('run-db', 'unavailable'), 'schemaSha256': HN}, opt=['forbiddenSource', 'schemaSha256']),
  'probe': probe,
})
doc = {'format': 'ui-run-schema/1',
       'about': 'Whitelist schema of ui-run/1 (tools/ui-bench/privacy.mjs). Every key is declared; strings are enums, sha256 digests or identity forms only.',
       'root': root, 'defs': defs}

def dump(x, ind=0):
    # ind = the column this value starts at; nested blocks indent two spaces per level.
    s = json.dumps(x)
    if len(s) + ind <= 118: return s
    pad = ' ' * (ind + 2)
    width = 116
    if isinstance(x, dict):
        lines, cur = [], ''
        for k, v in x.items():
            flat = json.dumps(k) + ': ' + json.dumps(v)
            if len(flat) <= 60:
                if cur and len(pad) + len(cur) + len(flat) > width:
                    lines.append(cur); cur = ''
                cur = flat if not cur else cur + ', ' + flat
                continue
            if cur: lines.append(cur); cur = ''
            lines.append(json.dumps(k) + ': ' + dump(v, ind + 2))
        if cur: lines.append(cur)
        return '{\n' + ',\n'.join(pad + l for l in lines) + '\n' + ' ' * ind + '}'
    if isinstance(x, list):
        if all(isinstance(v, str) for v in x):
            lines, cur = [], ''
            for v in x:
                piece = json.dumps(v)
                if cur and len(pad) + len(cur) + len(piece) > width:
                    lines.append(cur); cur = ''
                cur = piece if not cur else cur + ', ' + piece
            lines.append(cur)
            return '[\n' + ',\n'.join(pad + l for l in lines) + '\n' + ' ' * ind + ']'
        return '[\n' + ',\n'.join(pad + dump(v, ind + 2) for v in x) + '\n' + ' ' * ind + ']'
    return s
open(sys.argv[1], 'w').write(dump(doc) + '\n')

# ui-pair/1 (pair-eval.mjs): one BEFORE/AFTER pair or A/A campaign over ui-run/1
# documents. Written only when a second path is given:
#   python3 -I tools/ui-bench/build_schema.py tools/ui-bench/ui-run-1.schema.json tools/ui-bench/ui-pair-1.schema.json
# METRIC_IDS must equal pair-eval.mjs METRICS (test/pair-eval.test.mjs checks it).
SCRIPTS = ['reader-v1', 'passive-v1']
SIDES = ['before', 'after', 'a1', 'a2']
MILESTONE_METRICS = ['metadata-ready', 'first-readable', 'newest-giant-ready', 'recent-ready:24h@newest', 'recent-ready:7d@newest', 'canonical-ready', 'search-ready']
METRIC_IDS = ['H2.ms', 'H2.receiptMs', 'H3.h24.receiptMs', 'H3.h24.paintedSelMs', 'H3.h7d.receiptMs', 'H3.h7d.paintedSelMs', 'Hall.receiptMs',
  'Hall.paintedSelMs', 'H5.p95Ms', 'D2.inversionCorpusMs', 'D2.inversionSelectedMs', 'H4.p95Ms', 'H6.streaming.blockingMs', 'H6.finalizing.blockingMs',
  'H6.post.blockingMs', 'H1.ms', 'H1.metaPushMs', 'H1.statusPollMs', 'H2.lag.ms', 'H2.heldReadyMs', 'H4.p50Ms', 'H4.maxMs', 'H4.pendingFace.p50Ms',
  'H5.p50Ms', 'H5.maxMs', 'H6.streaming.longestMs', 'H6.finalizing.longestMs', 'H6.post.longestMs', 'H6.input.p98Ms', 'H6.input.maxMs', 'H6.heapPeakMb',
  'H6.appWorkingSetPeakMb', 'H6.probeMainPctOfWall', 'H7.projectsMoves', 'H7.sessionsMoves', 'H7.aimMisses', 'H7.selectionJumps', 'H8.chartsSelectedMs',
  'H8.chartsAllMs', 'H8.portfolioMs', 'H8.searchLagMs', 'D1.consentToProgressMs', 'D2.newestGiant.receiptMs', 'D3.wire.p95Ms'] + \
  ['daemon.' + k for k in MILESTONE_METRICS] + ['daemon.footprintMb', 'run.wallS']
VOID_REASONS = ['run-missing', 'wrong-document', 'run-not-complete', 'self-test-failed', 'i1-failed', 'privacy-failed', 'rosettad-differs-from-declared',
  'dist-differs-from-declared', 'identity-differs', 'source-differs-from-declared', 'script-differs', 'digest-missing', 'digest-differs']
EQUALITY_KEYS = ['logicalTotal', 'sourceSetDigest', 'rankedIdsSha256', 'hitDigest', 'finalRoots', 'finalReadySessions']
VERDICTS = ['pass', 'fail', 'incomparable', 'not-reached', 'not-applicable', 'report']
pair_stat = O({'n': NN, 'median': N, 'iqr': N, 'min': N, 'max': N, 'reached': O({'yes': NN, 'notReached': NN, 'notApplicable': NN})})
pair_source_snapshot = O({'walkInputsDigest': H, 'manifestSha256': H, 'files': NN, 'bytes': NN}, null=True)
pair_source_fixture = O({'name': E('ui-fixture-v1'), 'specSha256': H, 'treeSha256': H, 'files': NN, 'bytes': NN, 'walkInputsDigest': H,
  'generatorSha256': H, 'idleGuard': B}, null=True)
pair_root = O({
  'format': E('ui-pair/1'), 'gateSet': E('a'), 'kind': E('daemon', 'ux', 'aa'), 'campaignSha256': H,
  'startedAt': ID('iso-time', null=True), 'finishedAt': ID('iso-time', null=True),
  'order': E('abab', 'abba'), 'runsPerSide': NN, 'scripts': A(E(*SCRIPTS), 2),
  'sides': M(O({'rosettadSha256': H, 'distSha256': H, 'distFiles': N}), SIDES),
  'identity': O({'snapshot': pair_source_snapshot, 'fixture': pair_source_fixture, 'harnessSha256': M(H, HARNESS, null=True),
    'machine': O({'model': ID('machine-model'), 'os': ID('os-version'), 'cpus': NN, 'memGb': NN}, null=True), 'windowPx': ID('window', null=True),
    'dpr': N, 'dprMeasured': N, 'electron': ID('version', null=True), 'deviations': A(E(*DEVIATIONS), 16, null=True),
    'enrolled': A(E(*AGENTS), 16, null=True)}, null=True),
  'scriptSha256': M(HN, SCRIPTS),
  'options': O({'timeoutS': NN, 'idleMin': NN, 'guardWaitS': NN, 'idleGuard': B, 'minFreeGb': NN, 'dpr': NN, 'swapRedoMb': NN}),
  'claims': A(O({'script': E(*SCRIPTS), 'metric': E(*METRIC_IDS), 'maxRatio': N, 'maxP': N}), 64),
  'floors': O({'source': E('none', 'aa'), 'aaCampaignSha256': HN}),
  'equality': M(O({k: O({'value': {'anyOf': [HN, N]}, 'equal': B}) for k in EQUALITY_KEYS}), SCRIPTS),
  'void': B, 'voidReasons': A(E(*VOID_REASONS), 16),
  'runs': A(O({'slot': NN, 'round': NN, 'side': E(*SIDES), 'script': E(*SCRIPTS), 'discarded': B, 'present': B,
    'outcome': E('complete', 'incomplete', 'timeout', 'failed', null=True), 'selfTestPass': BN, 'i1Pass': BN, 'h7InvariantPass': BN, 'wallS': N,
    'cpuIdleAtStart': N, 'guardWaitedS': N, 'proceededOverGuard': BN, 'load1': N, 'swapMbBefore': N, 'swapMbAfter': N,
    'power': O({'source': E('ac', 'battery', 'unknown'), 'lowPowerMode': BN}), 'exitCode': N}), 2000),
  'metrics': A(O({'script': E(*SCRIPTS), 'metric': E(*METRIC_IDS), 'role': E('claim', 'guard', 'report-only'), 'before': pair_stat, 'after': pair_stat,
    'deltaMedian': N, 'ratio': N, 'toleranceMs': N, 'toleranceSource': E('none', 'before-iqr', 'aa-floor'), 'maxRatio': N, 'maxP': N,
    'mannWhitneyP': N, 'verdict': E(*VERDICTS), 'floorMs': N}, opt=['floorMs']), 1000),
  'summary': O({'verdict': E('pass', 'fail', 'void', 'aa'), 'claims': O({'pass': NN, 'fail': NN, 'incomparable': NN}),
    'guards': O({'pass': NN, 'fail': NN, 'incomparable': NN, 'notMeasured': NN}),
    'quiet': O({'runsOverGuard': NN, 'swapRedos': NN, 'maxSwapGrowthMb': N, 'onBattery': NN, 'lowPowerMode': NN})}),
})
if len(sys.argv) > 2:
    pair_doc = {'format': 'ui-pair-schema/1',
                'about': 'Whitelist schema of ui-pair/1 (tools/ui-bench/pair-eval.mjs, gated by privacy.mjs). Strings are enums, sha256 digests or identity forms only.',
                'root': pair_root, 'defs': {}}
    open(sys.argv[2], 'w').write(dump(pair_doc) + '\n')
