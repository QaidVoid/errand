import { assertEquals, assertRejects, assertStringIncludes, assertThrows } from "@std/assert";
import { join } from "@std/path";
import type { GithubConfig } from "../config/schema.ts";
import {
  type Api,
  currentBranch,
  findRepository,
  openPullRequest,
  parseRemote,
  pullRequestBody,
  PullRequestError,
  type Ran,
  type Run,
  upstream,
} from "./pr.ts";

const GITHUB: GithubConfig = {
  token: "ghp-value",
  userName: "errand-bot",
  userEmail: "bot@example.com",
};

function ok(stdout = ""): Ran {
  return { code: 0, stdout, stderr: "" };
}

const KNOWN_GIT = ["rev-parse", "remote", "push", "log"];

/** A git that answers by subcommand, and records everything it was asked. */
function fakeGit(answers: Record<string, Ran> = {}) {
  const calls: { args: string[]; env: Record<string, string> | undefined }[] = [];
  const run: Run = (command, options) => {
    calls.push({ args: command, env: options.env });
    const subcommand = command.find((word) => word in answers || KNOWN_GIT.includes(word)) ?? "";
    return Promise.resolve(answers[subcommand] ?? ok());
  };
  return { run, calls };
}

function fakeApi(answers: Record<string, { status: number; body: unknown }>) {
  const paths: string[] = [];
  const api: Api = (path, init) => {
    paths.push(`${init.method} ${path}`);
    return Promise.resolve(answers[`${init.method} ${path}`] ?? { status: 200, body: {} });
  };
  return { api, paths };
}

const FORKED = {
  status: 202,
  body: { name: "project-1", owner: { login: "errand-bot-login" } },
};

function workingApi() {
  return fakeApi({
    "POST /repos/upstream/project/forks": FORKED,
    "GET /repos/errand-bot-login/project-1": { status: 200, body: {} },
    "GET /repos/upstream/project": { status: 200, body: { default_branch: "trunk" } },
    "POST /repos/upstream/project/pulls": {
      status: 201,
      body: { html_url: "https://github.com/upstream/project/pull/7" },
    },
  });
}

function repoGit() {
  return fakeGit({
    "rev-parse": ok("feature/thing\n"),
    remote: ok("https://github.com/upstream/project.git\n"),
    log: ok("what changed and why\n"),
  });
}

async function withRepo(run: (project: string, repo: string) => Promise<void>): Promise<void> {
  const project = await Deno.makeTempDir({ prefix: "errand-pr-" });
  const repo = join(project, "project");
  Deno.mkdirSync(join(repo, ".git"), { recursive: true });
  try {
    await run(project, repo);
  } finally {
    await Deno.remove(project, { recursive: true });
  }
}

Deno.test("a remote is read in either form it was cloned in", () => {
  assertEquals(parseRemote("https://github.com/owner/repo.git"), {
    owner: "owner",
    name: "repo",
  });
  assertEquals(parseRemote("git@github.com:owner/repo.git"), { owner: "owner", name: "repo" });
  assertEquals(parseRemote("https://x-access-token:tok@github.com/owner/repo"), {
    owner: "owner",
    name: "repo",
  });
});

Deno.test("a remote that is not GitHub is not a repository this can open", () => {
  assertEquals(parseRemote("https://gitlab.com/owner/repo.git"), undefined);
  assertEquals(parseRemote("/srv/git/local.git"), undefined);
  assertEquals(parseRemote(""), undefined);
});

/**
 * The session directory is not the repository: whatever was asked for is
 * cloned into it. Looking only at the top reported that there was nothing to
 * open when there plainly was.
 */
Deno.test("the repository is found one level down from the session", () =>
  withRepo((project, repo) => {
    assertEquals(findRepository(project), repo);
    return Promise.resolve();
  }));

Deno.test("a session that is itself a repository is the repository", () =>
  withRepo((project) => {
    Deno.mkdirSync(join(project, ".git"), { recursive: true });
    assertEquals(findRepository(project), project);
    return Promise.resolve();
  }));

Deno.test("several repositories are named so somebody can say which", () =>
  withRepo((project) => {
    Deno.mkdirSync(join(project, "other", ".git"), { recursive: true });

    const error = assertThrows(() => findRepository(project), PullRequestError);

    assertStringIncludes(String(error), "other, project");
    assertEquals(findRepository(project, "other"), join(project, "other"));
    return Promise.resolve();
  }));

Deno.test("a session holding no repository says so plainly", () =>
  withRepo((project) => {
    Deno.removeSync(join(project, "project"), { recursive: true });

    assertStringIncludes(
      String(assertThrows(() => findRepository(project), PullRequestError)),
      "nothing in this session is a git repository",
    );
    return Promise.resolve();
  }));

/** A clone made as a worktree has `.git` as a file, not a directory. */
Deno.test("a worktree whose .git is a file still counts as one", () =>
  withRepo((project) => {
    Deno.mkdirSync(join(project, "linked"));
    Deno.writeTextFileSync(join(project, "linked", ".git"), "gitdir: /elsewhere\n");

    assertEquals(findRepository(project, "linked"), join(project, "linked"));
    return Promise.resolve();
  }));

Deno.test("a name that is a path is not the name of a repository", () =>
  withRepo((project) => {
    for (const name of ["../escape", ".", ".."]) {
      assertThrows(() => findRepository(project, name), PullRequestError);
    }
    return Promise.resolve();
  }));

Deno.test("a detached head has no branch to open a request for", async () => {
  const git = fakeGit({ "rev-parse": ok("HEAD\n") });

  assertStringIncludes(
    String(await assertRejects(() => currentBranch(git.run, "/p"), PullRequestError)),
    "no branch checked out",
  );
});

Deno.test("a project with no origin cannot say what to open against", async () => {
  const git = fakeGit({ remote: { code: 1, stdout: "", stderr: "no such remote" } });

  assertStringIncludes(
    String(await assertRejects(() => upstream(git.run, "/p"), PullRequestError)),
    "no origin remote",
  );
});

Deno.test("the body carries the summary and ends with the attribution", () => {
  const body = pullRequestBody("  it does the thing  ", "amelia", { thread: "https://t/1" });

  assertStringIncludes(body, "it does the thing");
  assertStringIncludes(body, "---");
  assertStringIncludes(body, "Requested by amelia via errand.");
  assertEquals(body.includes("@"), false);
});

Deno.test("a pull request is opened against the upstream from the bot's fork", () =>
  withRepo(async (project) => {
    const git = repoGit();
    const github = workingApi();

    const url = await openPullRequest(
      {
        github: GITHUB,
        projectPath: project,
        title: "Do the thing",
        requestedBy: "amelia",
        links: {},
      },
      git.run,
      github.api,
      () => Promise.resolve(),
    );

    assertEquals(url, "https://github.com/upstream/project/pull/7");
    assertStringIncludes(github.paths.join("\n"), "POST /repos/upstream/project/forks");
    assertStringIncludes(github.paths.join("\n"), "POST /repos/upstream/project/pulls");
  }));

/**
 * The fork's own name and owner are read from the answer. An account that
 * already holds a repository of that name gets the fork under another, and the
 * owner is a login, which the configured author name need not be.
 */
Deno.test("the push goes to the fork GitHub actually made", () =>
  withRepo(async (project) => {
    const git = repoGit();
    const github = workingApi();

    await openPullRequest(
      { github: GITHUB, projectPath: project, title: "t", requestedBy: "amelia", links: {} },
      git.run,
      github.api,
      () => Promise.resolve(),
    );

    const push = git.calls.find((call) => call.args.includes("push"));
    assertStringIncludes(
      push?.args.join(" ") ?? "",
      "https://github.com/errand-bot-login/project-1.git",
    );
    assertStringIncludes(push?.args.join(" ") ?? "", "HEAD:refs/heads/feature/thing");
  }));

/** In `ps` for anyone on the host, and in git's own error output. */
Deno.test("the token is passed in the environment, never on a command line", () =>
  withRepo(async (project) => {
    const git = repoGit();
    const github = workingApi();

    await openPullRequest(
      { github: GITHUB, projectPath: project, title: "t", requestedBy: "amelia", links: {} },
      git.run,
      github.api,
      () => Promise.resolve(),
    );

    const push = git.calls.find((call) => call.args.includes("push"));
    assertEquals(push?.args.join(" ").includes(GITHUB.token), false);
    assertEquals(push?.env?.ERRAND_GH_TOKEN, GITHUB.token);
    assertStringIncludes(push?.args.join(" ") ?? "", "$ERRAND_GH_TOKEN");
  }));

Deno.test("the request is opened against whatever the upstream merges into", () =>
  withRepo(async (project) => {
    const git = repoGit();
    const github = fakeApi({
      "POST /repos/upstream/project/forks": FORKED,
      "GET /repos/errand-bot-login/project-1": { status: 200, body: {} },
      "GET /repos/upstream/project": { status: 200, body: { default_branch: "develop" } },
      "POST /repos/upstream/project/pulls": { status: 201, body: { html_url: "https://x/1" } },
    });
    let opened: Record<string, unknown> = {};
    const watching: Api = (path, init) => {
      if (path.endsWith("/pulls")) opened = init.body as Record<string, unknown>;
      return github.api(path, init);
    };

    await openPullRequest(
      { github: GITHUB, projectPath: project, title: "t", requestedBy: "amelia", links: {} },
      git.run,
      watching,
      () => Promise.resolve(),
    );

    assertEquals(opened.base, "develop");
    assertEquals(opened.head, "errand-bot-login:feature/thing");
  }));

/**
 * Forking is asynchronous and answered before it has finished. Pushing inside
 * that window fails as though the repository did not exist, which is what this
 * poll is for.
 */
Deno.test("a fork that is not there yet is waited for", () =>
  withRepo(async (project) => {
    const git = repoGit();
    let looks = 0;
    const api: Api = (path, init) => {
      if (path === "/repos/errand-bot-login/project-1" && init.method === "GET") {
        looks += 1;
        return Promise.resolve({ status: looks < 3 ? 404 : 200, body: {} });
      }
      return workingApi().api(path, init);
    };
    let waited = 0;

    await openPullRequest(
      { github: GITHUB, projectPath: project, title: "t", requestedBy: "amelia", links: {} },
      git.run,
      api,
      () => {
        waited += 1;
        return Promise.resolve();
      },
    );

    assertEquals(looks, 3);
    assertEquals(waited, 2);
  }));

/** A repository the token cannot see is reported as missing, not forbidden. */
Deno.test("a fork that is refused says the token may not reach the repository", () =>
  withRepo(async (project) => {
    const git = repoGit();
    const github = fakeApi({
      "POST /repos/upstream/project/forks": { status: 404, body: { message: "Not Found" } },
    });

    const error = await assertRejects(
      () =>
        openPullRequest(
          { github: GITHUB, projectPath: project, title: "t", requestedBy: "a", links: {} },
          git.run,
          github.api,
          () => Promise.resolve(),
        ),
      PullRequestError,
    );

    assertStringIncludes(String(error), "may not have access");
  }));

Deno.test("a push that fails says why, in one line worth posting", () =>
  withRepo(async (project) => {
    const git = fakeGit({
      "rev-parse": ok("feature/thing\n"),
      remote: ok("https://github.com/upstream/project.git\n"),
      push: { code: 1, stdout: "", stderr: "! [rejected] stale info\nhint: read this\n" },
    });
    const github = workingApi();

    const error = await assertRejects(
      () =>
        openPullRequest(
          { github: GITHUB, projectPath: project, title: "t", requestedBy: "a", links: {} },
          git.run,
          github.api,
          () => Promise.resolve(),
        ),
      PullRequestError,
    );

    assertStringIncludes(String(error), "rejected");
    assertEquals(String(error).includes("hint:"), false);
  }));

Deno.test("a request GitHub refuses is reported with what it said", () =>
  withRepo(async (project) => {
    const git = repoGit();
    const github = fakeApi({
      "POST /repos/upstream/project/forks": FORKED,
      "GET /repos/errand-bot-login/project-1": { status: 200, body: {} },
      "POST /repos/upstream/project/pulls": {
        status: 422,
        body: { message: "A pull request already exists" },
      },
    });

    assertStringIncludes(
      String(
        await assertRejects(
          () =>
            openPullRequest(
              { github: GITHUB, projectPath: project, title: "t", requestedBy: "a", links: {} },
              git.run,
              github.api,
              () => Promise.resolve(),
            ),
          PullRequestError,
        ),
      ),
      "already exists",
    );
  }));
