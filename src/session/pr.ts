/**
 * Opening a pull request for a session's work.
 *
 * Done by the daemon rather than by the agent, so what a pull request says
 * about where it came from is not left to a model's discretion. An instruction
 * in a prompt is advice: one was seen keeping the two lines that read as
 * useful and dropping the third as redundant. This composes the body itself.
 *
 * The agent does hold a token, because reading issues and checking builds
 * needs one, so this is a route rather than a wall. What it buys is that the
 * ordinary path produces a correct description every time, and that the daemon
 * is the one that reports where the pull request went.
 */

import { join } from "@std/path";
import type { GithubConfig } from "../config/schema.ts";
import { attributionFooter, type SessionLinks } from "./github.ts";

/** A repository on GitHub, as the API addresses it. */
export interface Repo {
  owner: string;
  name: string;
}

/**
 * Reads the owner and repository out of a remote URL.
 *
 * Both forms are accepted because a project may have been cloned either way,
 * and the daemon has no say in which.
 *
 * @returns undefined for anything that is not GitHub, since there is nothing
 *   useful to do with it here.
 */
export function parseRemote(url: string): Repo | undefined {
  const trimmed = url.trim().replace(/\.git$/, "");
  const https = /^https?:\/\/(?:[^@/]*@)?github\.com\/([^/]+)\/([^/]+)$/.exec(trimmed);
  if (https !== null) return { owner: https[1] as string, name: https[2] as string };
  const ssh = /^(?:ssh:\/\/)?git@github\.com[:/]([^/]+)\/([^/]+)$/.exec(trimmed);
  if (ssh !== null) return { owner: ssh[1] as string, name: ssh[2] as string };
  return undefined;
}

/** What a command said. */
export interface Ran {
  code: number;
  stdout: string;
  stderr: string;
}

/** Runs a command. Injected so tests need no repository and no network. */
export type Run = (
  command: string[],
  options: { cwd?: string; env?: Record<string, string> },
) => Promise<Ran>;

/** Waits. Injected so a test does not sit through a fork appearing. */
export type Sleep = (ms: number) => Promise<void>;

/** Calls the GitHub API. Injected for the same reason. */
export type Api = (
  path: string,
  init: { method: string; token: string; body?: unknown },
) => Promise<{ status: number; body: unknown }>;

/** What went wrong, in words worth posting into a thread. */
export class PullRequestError extends Error {}

/** What a session produced, and where it should go. */
export interface Request {
  github: GithubConfig;
  /** The session's directory, which holds the repository rather than being one. */
  projectPath: string;
  /** Which repository in it, by directory name. Only needed when it holds several. */
  repository?: string | undefined;
  /** Title for the pull request. */
  title: string;
  /** Who asked, and where the conversation is. */
  requestedBy: string;
  links: SessionLinks;
}

/** The body the daemon writes, which is the whole point of doing this here. */
export function pullRequestBody(summary: string, requestedBy: string, links: SessionLinks): string {
  return `${summary.trim()}\n\n---\n\n${attributionFooter(requestedBy, links)}\n`;
}

/**
 * Pushes without the token ever reaching a command line.
 *
 * A URL carrying it would appear in `ps` for anyone on the host, and in git's
 * own error output. A credential helper reads it from the environment instead.
 */
const TOKEN_VARIABLE = "ERRAND_GH_TOKEN";
const CREDENTIAL_HELPER =
  `!f() { echo "username=x-access-token"; echo "password=$${TOKEN_VARIABLE}"; }; f`;

/**
 * Settings a repository must not be allowed to supply.
 *
 * These commands run on the host, outside the sandbox, against a tree the
 * session can write. Git treats a repository as a source of code as much as of
 * data: a hook, a credential helper, a filesystem monitor are all commands it
 * will run on the daemon's behalf. Naming each one on the command line beats
 * whatever the repository says, since a `-c` is read last.
 *
 * Overriding named settings is a denylist, and a denylist against git is a
 * losing game: signature verification runs `gpg.program`, a textconv filter
 * runs its command, and the list grows with git. So this is only the first
 * layer. The push happens in a clone the session never wrote, and anything
 * that reads a commit reads it there too, where the repository's configuration
 * is gone rather than merely overridden. See {@link pushWork}.
 */
const SAFE_CONFIG: readonly string[] = [
  "-c",
  "core.hooksPath=/dev/null",
  "-c",
  "core.fsmonitor=false",
  "-c",
  "core.pager=cat",
  "-c",
  "credential.helper=",
  "-c",
  "http.proxy=",
  "-c",
  "http.sslVerify=true",
  "-c",
  "protocol.ext.allow=never",
  // A signed commit is verified by running the configured "gpg", so a
  // repository that says what that program is runs it. Nothing here needs a
  // signature checked, so none is.
  "-c",
  "log.showSignature=false",
  "-c",
  "merge.verifySignatures=false",
];

/**
 * The environment git is given, so the host's own configuration cannot join in.
 *
 * The system and global files are the caller's rather than the repository's,
 * but neither is wanted here: this runs one known operation and should behave
 * the same on every host. A prompt would hang a daemon nobody is watching.
 */
const SAFE_ENV: Readonly<Record<string, string>> = {
  GIT_CONFIG_NOSYSTEM: "1",
  GIT_CONFIG_GLOBAL: "/dev/null",
  GIT_TERMINAL_PROMPT: "0",
};

function git(run: Run, cwd: string, args: string[], token?: string): Promise<Ran> {
  const env: Record<string, string> = { ...SAFE_ENV };
  if (token !== undefined) env[TOKEN_VARIABLE] = token;
  return run(["git", ...SAFE_CONFIG, ...args], { cwd, env });
}

/** The branch the work is on, refusing a detached head. */
export async function currentBranch(run: Run, projectPath: string): Promise<string> {
  const head = await git(run, projectPath, ["rev-parse", "--abbrev-ref", "HEAD"]);
  if (head.code !== 0) {
    throw new PullRequestError(`git could not read a branch in ${projectPath}`);
  }
  const branch = head.stdout.trim();
  if (branch === "HEAD") throw new PullRequestError("this project has no branch checked out");
  return branch;
}

/** The upstream this work came from, read from the remote the agent cloned. */
export async function upstream(run: Run, projectPath: string): Promise<Repo> {
  const remote = await git(run, projectPath, ["remote", "get-url", "origin"]);
  if (remote.code !== 0) {
    throw new PullRequestError("this project has no origin remote to open a pull request against");
  }
  const repo = parseRemote(remote.stdout);
  if (repo === undefined) {
    throw new PullRequestError(`origin is not a GitHub remote: ${remote.stdout.trim()}`);
  }
  return repo;
}

/**
 * Whether a directory is a working tree.
 *
 * Tested by existence rather than by type, because a clone made as a worktree
 * or a submodule has `.git` as a file pointing elsewhere.
 */
function isWorkTree(path: string): boolean {
  try {
    Deno.lstatSync(join(path, ".git"));
    return true;
  } catch {
    return false;
  }
}

function repositoriesIn(projectPath: string): string[] {
  try {
    return [...Deno.readDirSync(projectPath)]
      .filter((entry) => entry.isDirectory && isWorkTree(join(projectPath, entry.name)))
      .map((entry) => entry.name)
      .sort();
  } catch {
    return [];
  }
}

/**
 * Finds the working tree the pull request is for.
 *
 * A session's directory is not itself a repository. Whatever the agent was
 * asked to work on is cloned into it, so the repository is normally one level
 * down, and looking only at the top would report that there is nothing to open
 * when there plainly is.
 *
 * `named` picks between them, which a session reused under the same name for a
 * while needs.
 */
export function findRepository(projectPath: string, named?: string): string {
  if (named !== undefined && named.length > 0) {
    if (named.includes("/") || named === "." || named === "..") {
      throw new PullRequestError(`\`${named}\` is not the name of a repository in this session`);
    }
    const chosen = join(projectPath, named);
    if (!isWorkTree(chosen)) {
      throw new PullRequestError(`there is no repository called \`${named}\` in this session`);
    }
    return chosen;
  }

  if (isWorkTree(projectPath)) return projectPath;

  const found = repositoriesIn(projectPath);
  if (found.length === 1) return join(projectPath, found[0] as string);
  if (found.length === 0) {
    throw new PullRequestError(
      "nothing in this session is a git repository yet, so there is nothing to open",
    );
  }
  throw new PullRequestError(
    `this session holds several repositories (${found.join(", ")}), so say which one to open`,
  );
}

/** Longest wait for a new fork to appear, and how often it is looked for. */
export const FORK_WAIT_MS = 30_000;
const FORK_POLL_MS = 1_000;

/** Reads a repository out of an API answer that describes one. */
function named(body: unknown): Repo | undefined {
  const repo = body as { name?: unknown; owner?: { login?: unknown } };
  if (typeof repo.name !== "string" || typeof repo.owner?.login !== "string") return undefined;
  return { owner: repo.owner.login, name: repo.name };
}

/**
 * The bot's fork of the upstream, once it is there to push to.
 *
 * Where it lands is read from GitHub rather than assumed. The name is not
 * always the upstream's, since an account already holding a repository of that
 * name gets the fork under a different one, and the owner is a login, which is
 * not what the configured author name has to be.
 *
 * Waiting is the other half. Forking is asynchronous and answered before it
 * has finished, and pushing inside that window fails as though the repository
 * did not exist.
 */
async function forkOf(api: Api, token: string, target: Repo, sleep: Sleep): Promise<Repo> {
  const made = await api(`/repos/${target.owner}/${target.name}/forks`, { method: "POST", token });
  if (made.status >= 400) {
    // A repository the token cannot see is reported as missing rather than as
    // forbidden, so the two are worth naming together.
    const reach = made.status === 403 || made.status === 404
      ? ", which the bot's token may not have access to"
      : "";
    throw new PullRequestError(
      `could not fork ${target.owner}/${target.name}${reach}: ${detail(made.body)}`,
    );
  }

  const fork = named(made.body);
  if (fork === undefined) {
    throw new PullRequestError("GitHub accepted the fork but did not say where it put it");
  }

  const deadline = Date.now() + FORK_WAIT_MS;
  for (;;) {
    const there = await api(`/repos/${fork.owner}/${fork.name}`, { method: "GET", token });
    if (there.status === 200) return fork;
    if (Date.now() >= deadline) {
      throw new PullRequestError(
        `the fork ${fork.owner}/${fork.name} did not become available to push to`,
      );
    }
    await sleep(FORK_POLL_MS);
  }
}

/** What the upstream merges into, which is not always `main`. */
async function defaultBranch(api: Api, token: string, repo: Repo): Promise<string> {
  const answer = await api(`/repos/${repo.owner}/${repo.name}`, { method: "GET", token });
  const branch = (answer.body as { default_branch?: unknown }).default_branch;
  return typeof branch === "string" ? branch : "main";
}

function detail(body: unknown): string {
  const message = (body as { message?: unknown }).message;
  return typeof message === "string" ? message : "GitHub refused it";
}

function firstLine(text: string): string {
  return text.trim().split("\n")[0] ?? "it did not say why";
}

/**
 * Pushes the session's commit from a repository it never had a chance to write.
 *
 * The push is the one step that carries the token and opens a connection, and
 * it is the step git hangs the most on a repository's own configuration: a
 * `pre-push` hook, a credential helper named for a single URL, an `insteadOf`
 * that sends the whole thing somewhere else. A session owns its working tree,
 * so against that tree none of those can be trusted, and the last two cannot be
 * overridden from the command line at all.
 *
 * So the commit is pushed from a bare repository made here, holding one ref and
 * a pointer to the session's objects. Objects are data and are only ever read;
 * the configuration, which is code, is left behind. The session never learns of
 * this directory and cannot write to it.
 */
async function pushWork(
  run: Run,
  projectPath: string,
  branch: string,
  url: string,
  token: string,
): Promise<string> {
  const staging = await Deno.makeTempDir({ prefix: "errand-push-" });
  const repository = join(staging, "repository.git");
  try {
    // A clone takes the refs and leaves the configuration: the copy gets a
    // fresh one, and hooks are never carried over. `--shared` borrows the
    // objects rather than copying them, so a long history costs nothing here,
    // and objects are read-only data in any case.
    const cloned = await git(run, staging, [
      "clone",
      "--shared",
      "--bare",
      "--quiet",
      projectPath,
      repository,
    ]);
    if (cloned.code !== 0) {
      throw new PullRequestError(`could not prepare the push: ${firstLine(cloned.stderr)}`);
    }

    const pushed = await git(run, repository, [
      "-c",
      `credential.helper=${CREDENTIAL_HELPER}`,
      "push",
      "--force-with-lease",
      url,
      `refs/heads/${branch}:refs/heads/${branch}`,
    ], token);
    if (pushed.code !== 0) {
      throw new PullRequestError(`could not push ${branch}: ${firstLine(pushed.stderr)}`);
    }

    // Read here, where the configuration is the clone's own rather than the
    // session's. Reading it in the session's tree is what let a crafted commit
    // and a `gpg.program` in its config run on the host: `git log` verifies a
    // signature by running that program. The clone carries neither.
    const summary = await git(run, repository, ["log", "-1", "--format=%b"]);
    return summary.stdout;
  } finally {
    await Deno.remove(staging, { recursive: true }).catch(() => {});
  }
}

/**
 * Opens the pull request, and returns where it is.
 *
 * The fork is made first and pushed to, rather than pushing to the upstream: a
 * bot that never needs write access to somebody else's repository cannot lose
 * it.
 */
export async function openPullRequest(
  request: Request,
  run: Run,
  api: Api,
  sleep: Sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
): Promise<string> {
  const { github } = request;
  const projectPath = findRepository(request.projectPath, request.repository);
  const branch = await currentBranch(run, projectPath);
  const target = await upstream(run, projectPath);
  const fork = await forkOf(api, github.token, target, sleep);

  const summary = await pushWork(
    run,
    projectPath,
    branch,
    `https://github.com/${fork.owner}/${fork.name}.git`,
    github.token,
  );

  const created = await api(`/repos/${target.owner}/${target.name}/pulls`, {
    method: "POST",
    token: github.token,
    body: {
      title: request.title,
      head: `${fork.owner}:${branch}`,
      base: await defaultBranch(api, github.token, target),
      body: pullRequestBody(summary, request.requestedBy, request.links),
      maintainer_can_modify: true,
    },
  });
  if (created.status >= 400) {
    throw new PullRequestError(`could not open the pull request: ${detail(created.body)}`);
  }

  const url = (created.body as { html_url?: unknown }).html_url;
  if (typeof url !== "string") {
    throw new PullRequestError("the pull request was created but GitHub did not say where");
  }
  return url;
}

/** Runs a command on the host, for the daemon's own git operations. */
export const runCommand: Run = async function runCommand(
  command: string[],
  options: { cwd?: string; env?: Record<string, string> },
): Promise<Ran> {
  const [program, ...args] = command;
  const output = await new Deno.Command(program as string, {
    args,
    stdout: "piped",
    stderr: "piped",
    ...(options.cwd === undefined ? {} : { cwd: options.cwd }),
    // The daemon's environment holds the bot token and the chat one. Only what
    // is named here crosses into git.
    clearEnv: true,
    env: {
      PATH: Deno.env.get("PATH") ?? "",
      HOME: Deno.env.get("HOME") ?? "",
      ...options.env,
    },
  }).output();

  return {
    code: output.code,
    stdout: new TextDecoder().decode(output.stdout),
    stderr: new TextDecoder().decode(output.stderr),
  };
};

/** Calls the GitHub REST API as the bot. */
export const callApi: Api = async function callApi(
  path: string,
  init: { method: string; token: string; body?: unknown },
): Promise<{ status: number; body: unknown }> {
  const response = await fetch(`https://api.github.com${path}`, {
    method: init.method,
    headers: {
      Authorization: `Bearer ${init.token}`,
      Accept: "application/vnd.github+json",
      "X-GitHub-Api-Version": "2022-11-28",
      ...(init.body === undefined ? {} : { "Content-Type": "application/json" }),
    },
    ...(init.body === undefined ? {} : { body: JSON.stringify(init.body) }),
    signal: AbortSignal.timeout(30_000),
  });
  return { status: response.status, body: await response.json().catch(() => ({})) };
};
