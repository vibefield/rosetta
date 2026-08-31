// The git half of column ③ — what the repository has not recorded yet, and
// what it has. The column that hosts these lives in `components/worktree`,
// because files, changes and history are three views of one checkout.

export { ChangeTree } from "./ChangeTree";
export { creditedAgents } from "./co-author";
export { CommitViewer } from "./CommitViewer";
export { createGitFileSource, diffHandle } from "./git-file-source";
export { useGitClient, useGitRead } from "./git-host";
export { changeShapeOf, GitChanges } from "./GitChanges";
export { GitHistory } from "./GitHistory";
export { buildPathTree, filesUnder } from "./path-tree";
export { sessionFilePaths } from "./session-files";
export type {
  BranchFacts,
  CoAuthor,
  Commit,
  CommitFile,
  GitClient,
  StatusEntry,
  StatusLetter,
} from "./types";
