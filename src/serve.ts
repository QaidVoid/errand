/**
 * Starting the daemon: what runs, and in what order.
 *
 * Order matters. Configuration is validated, the lock is taken, the backend is
 * probed and its gaps reported, leftover sandboxes are swept, and only then
 * does the connection open. Nothing that could start an agent happens before
 * the isolation contract has been checked and reported.
 */

import { dirname, fromFileUrl, join } from "@std/path";
import { ChannelType } from "discord.js";
import { acknowledge, applicationId, registerCommands } from "./chat/commands.ts";
import { Gateway } from "./chat/gateway.ts";
import { whenRelative, whenRelativePlain } from "./chat/render.ts";
import { assertChannelUsable, ChatThreadFactory, plain } from "./chat/threads.ts";
import { redactText, secretValues } from "./config/redact.ts";
import type { Config } from "./config/schema.ts";
import { createSandbox, Daemon, probeSandbox } from "./daemon.ts";
import { Broker, type ProviderRoute } from "./sandbox/broker.ts";
import {
  EGRESS_MAP_ADDRESS,
  PROVIDER_PREFIX,
  type ProviderBrokering,
  providerBrokerUrl,
} from "./sandbox/bailey.ts";
import { acquireLock } from "./lock.ts";
import type { Logger } from "./log.ts";
import { MemoryStore } from "./memory/store.ts";
import { agentDirectory, modelById, readModels } from "./provider/models.ts";
import { imageDescriber } from "./provider/vision.ts";
import {
  isSpent,
  metersUsage,
  QUOTA_TTL_MS,
  QuotaGate,
  quotaMessage,
  quotaStatus,
  spentMessage,
  UNKNOWN_QUOTA,
} from "./provider/zai.ts";
import type { IncomingMessage } from "./session/session.ts";
import { WEB_ACTOR, WebServer } from "./web/server.ts";

/** Filename of the memory database inside the state directory. */
export const MEMORY_FILENAME = "memory.db";

/**
 * Powers off through logind, which is what a desktop session uses.
 *
 * Its policy allows an active local session and asks for authentication
 * otherwise, so this works when the daemon was started from a logged-in seat
 * and fails with a reason when it was not.
 */
async function powerOff(): Promise<string | undefined> {
  const output = await new Deno.Command("loginctl", {
    args: ["poweroff"],
    stdout: "piped",
    stderr: "piped",
  }).output();
  if (output.code === 0) return undefined;

  const said = new TextDecoder().decode(output.stderr).trim().split("\n")[0];
  return `could not power off: ${
    said === undefined || said.length === 0 ? `it exited with ${output.code}` : said
  }`;
}

/**
 * Runs the daemon until it is told to stop.
 *
 * Serving is the steady state, so this resolves only on a signal or when the
 * connection has been lost for good.
 */
/**
 * A per-run stand-in for the provider credential.
 *
 * Drawn from the system generator rather than anything derived from the
 * credential, so holding the nonce says nothing about the key it stands for. It
 * lasts as long as the daemon: the broker is the only thing that honours it,
 * and a restart brings a new one.
 */
function providerNonce(): string {
  const bytes = new Uint8Array(32);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

/** The lower-cased host of a base URL, for the egress allowlist. */
function hostOf(baseUrl: string): string | undefined {
  try {
    return new URL(baseUrl).hostname.toLowerCase();
  } catch {
    return undefined;
  }
}

export async function serve(config: Config, log: Logger): Promise<number> {
  const secrets = secretValues(config);

  // Taken before anything connects or spawns, so a second daemon fails fast
  // instead of racing the first one for every message that arrives.
  Deno.mkdirSync(config.stateDir, { recursive: true });
  const lock = acquireLock(config.stateDir);

  try {
    return await run(config, log, secrets, lock.release);
  } finally {
    // Released on every path, including a startup that never got as far as
    // connecting. A lock left behind is taken over next time because its
    // holder is gone, but leaving one is still a puzzle for whoever finds it.
    lock.release();
  }
}

/** Everything between taking the lock and giving it back. */
async function run(
  config: Config,
  log: Logger,
  secrets: readonly string[],
  releaseLock: () => void,
): Promise<number> {
  // Named but unreadable is a refusal, not a warning. An operator who config-
  // ured house rules believes every session carries them, and a typo that only
  // ever showed up as a line in a log would leave that belief standing while
  // no session actually got them.
  const rulesPath = config.agent.rulesPath;
  if (rulesPath !== undefined) {
    try {
      Deno.readTextFileSync(rulesPath);
      log.info("house rules will be given to every session", { path: rulesPath });
    } catch (error) {
      log.error(`agent.rulesPath cannot be read: ${rulesPath}`);
      log.error(String(error));
      return 2;
    }
  }

  // Under `egress.mode = proxy` every session's outbound is forced through one
  // broker the daemon runs here on the host. Its allowlist is the provider,
  // which a session cannot work without, plus whatever the operator named. The
  // provider host is read from the model store the agent would reach it at.
  let broker: Broker | undefined;
  let egressProxyPort: number | undefined;
  let brokering: ProviderBrokering | undefined;
  let route: ProviderRoute | undefined;
  if (config.sandbox.egress.mode === "proxy") {
    const store = agentDirectory(Deno.env.toObject());
    const providerBase = modelById(readModels(store, config.agent.provider), config.agent.model)
      ?.baseUrl;
    const providerHost = providerBase === undefined ? undefined : hostOf(providerBase);
    const allow = [
      ...(providerHost === undefined ? [] : [providerHost]),
      ...config.sandbox.egress.allow,
    ];
    if (allow.length === 0) {
      log.error(
        "egress.mode is proxy but no host is allowed: name the provider host or set egress.allow",
      );
      return 2;
    }
    // The credential is held back from the session and put on here instead, so
    // what a sandbox carries is a nonce that is worth nothing anywhere else.
    // Only where the provider's own base URL is known, since the broker has to
    // know where to forward to.
    if (providerBase !== undefined) {
      brokering = { credentialName: config.agent.credentialName, nonce: providerNonce() };
      route = {
        prefix: PROVIDER_PREFIX,
        upstream: providerBase,
        nonce: brokering.nonce,
        credential: config.agent.credential,
      };
    }
    broker = new Broker(allow, log, route);
    egressProxyPort = broker.listen();
    log.info("egress is brokered", {
      via: `${EGRESS_MAP_ADDRESS}:${egressProxyPort}`,
      allow: allow.join(", "),
    });
    if (route !== undefined) {
      log.info("the provider credential stays outside the sandbox", {
        via: providerBrokerUrl(egressProxyPort),
      });
    } else {
      log.warn(
        "the model store does not say where the provider is, so the credential is given to the session",
      );
    }
  }

  // The sandbox is checked before the chat service is touched, so a missing
  // image or an unenforceable guarantee fails immediately rather than after a
  // login round trip.
  const sandbox = createSandbox(config, log, egressProxyPort, brokering);
  const report = await probeSandbox(sandbox, config, log);

  // The connection signals readiness during connect(), before the thread
  // factory and the daemon exist, so the handlers reach them through holders
  // rather than closing over bindings that are not initialised yet.
  let threads: ChatThreadFactory | null = null;
  let daemon: Daemon | null = null;
  // A reconnect resets the presence, so the status is put back by the same
  // call that first set it rather than only at startup.
  let refreshStatus: () => void = () => {};
  let stop: (reason: string) => void = () => {};
  const stopped = new Promise<string>((resolve) => {
    stop = resolve;
  });

  const gateway = new Gateway(config.chat, {
    onMessage: async (raw, decision) => {
      await daemon?.handle(raw, decision);
    },
    onCommand: async (command, interaction) => {
      await acknowledge(
        interaction,
        (await daemon?.runCommand(command)) ?? "the daemon is not ready",
      );
    },
    onThreadClosed: async (threadId) => {
      await daemon?.threadClosed(threadId);
    },
    onConnected: () => {
      threads?.setConnected(true);
      refreshStatus();
      log.info("connected");
    },
    onDisconnected: () => {
      threads?.setConnected(false);
      log.warn("disconnected; sessions keep running and output is buffered");
    },
    onGaveUp: (attempts) => {
      log.error("reconnection failed for good; shutting sessions down", { attempts });
      stop("the connection was lost");
    },
  }, log);

  await gateway.connect();
  await assertChannelUsable(gateway.connection, config.chat.channelId);

  threads = new ChatThreadFactory(
    gateway.connection,
    config.chat.channelId,
    log,
    config.output.forwardToolOutput,
  );

  const memory = new MemoryStore(join(config.stateDir, MEMORY_FILENAME));

  // Only for a provider that meters a window. Everywhere else there is nothing
  // to ask and nothing to refuse against.
  const quota = metersUsage(config.agent.provider)
    ? new QuotaGate(config.agent.credential)
    : undefined;

  // What is left of the window is the thing somebody wants to know BEFORE
  // asking for work, and the member list is where they look first. The gate
  // holds its answer for QUOTA_TTL_MS and stops asking entirely once the
  // window is spent, so refreshing on that same interval adds no requests the
  // daemon was not already making. A window that cannot be read clears the
  // status rather than leaving a stale number under the bot's name.
  let statusTimer: ReturnType<typeof setInterval> | undefined;
  if (quota !== undefined) {
    refreshStatus = () => {
      void (async () => {
        const window = await quota.current();
        gateway.setStatus(
          window === undefined
            ? undefined
            : quotaStatus(window, whenRelativePlain(window.resetsAt)),
        );
      })();
    };
    refreshStatus();
    statusTimer = setInterval(refreshStatus, QUOTA_TTL_MS);
  }

  // Decided once: what a model accepts is the provider's business, not
  // something to work out per attachment.
  const store = agentDirectory(Deno.env.toObject());
  const models = readModels(store, config.agent.provider);

  // Where a delegated question goes: the endpoint this provider is already
  // reached at, taken from the model named for it or from the session's own.
  const delegate = config.agent.delegate;
  const delegateBaseUrl = delegate === undefined ? undefined : (delegate.baseUrl ??
    modelById(models, delegate.model)?.baseUrl ??
    modelById(models, config.agent.model)?.baseUrl);
  if (delegate !== undefined && delegateBaseUrl === undefined) {
    log.warn("delegation is configured but there is nowhere to send it", {
      model: delegate.model,
      detail: "set agent.delegate.baseUrl, or install the agent's model store on this host",
    });
  }

  const describer = imageDescriber(config.agent, store);
  if (describer !== undefined) {
    log.info("images will be described for this model", {
      model: config.agent.model ?? "",
      by: describer.model,
    });
  }

  // A request from the interface acts as an operator, which is the authority
  // that reaching a private listener already implies. An observer gets none.
  const webOperates = config.web !== undefined && !config.web.observer;

  daemon = new Daemon({
    config,
    sandbox,
    threads,
    log,
    memory,
    powerOff,
    ...(config.web?.publicUrl === undefined ? {} : { publicUrl: config.web.publicUrl }),
    operatorIds: webOperates
      ? [...config.chat.operatorUserIds, WEB_ACTOR]
      : config.chat.operatorUserIds,
    availableModels: models.map((model) => model.id),
    ...(delegateBaseUrl === undefined ? {} : { delegateBaseUrl }),
    ...(describer === undefined ? {} : { describeImages: describer.describe }),
    ...(quota === undefined ? {} : {
      // Checked before a thread is opened or a sandbox started, so a spent
      // window is answered with when to come back rather than with a turn
      // that starts and then fails against the provider.
      unavailable: async () => {
        const window = await quota.current();
        return window === undefined || !isSpent(window)
          ? undefined
          : spentMessage(whenRelative(window.resetsAt));
      },
      describeUsage: async () => {
        const window = await quota.current();
        return window === undefined
          ? UNKNOWN_QUOTA
          : quotaMessage(window, whenRelative(window.resetsAt));
      },
    }),
    replyInChannel: async (message: IncomingMessage, text: string) => {
      const channel = await gateway.connection.channels.fetch(config.chat.channelId);
      if (channel === null || channel.type !== ChannelType.GuildText) return;
      const starter = await channel.messages.fetch(message.id).catch(() => null);
      await starter?.reply(plain(redactText(text, secrets))).catch(() => undefined);
    },
  });

  await daemon.start(report);

  // Registered after startup, so a bot invited without the commands scope
  // reports that clearly instead of failing before it can serve anything.
  const channel = await gateway.connection.channels.fetch(config.chat.channelId);
  const guild = channel !== null && "guild" in channel ? channel.guild : null;
  if (guild !== null) daemon.setGuild(guild.id);

  const appId = applicationId(gateway.connection);
  if (guild !== null && appId !== undefined) {
    try {
      await registerCommands(config.chat.token, appId, guild, log);
    } catch (error) {
      log.warn(String(error));
      log.warn("slash commands are unavailable; the ! commands still work");
    }
  }

  // Started after the daemon is accepting, so the interface never lists a
  // session the daemon is not yet ready to act on.
  let web: WebServer | null = null;
  if (config.web !== undefined) {
    web = new WebServer(
      config.web,
      daemon.sessions,
      join(dirname(fromFileUrl(import.meta.url)), "..", "dist", "web"),
      log,
      guild?.id,
      (id) => memory.displayName(id),
    );
    try {
      web.start();
      log.info("ACCESS: anyone who can reach the interface acts with operator authority");
      log.info("  the agent's sandbox is unaffected by this; the interface is not sandboxed");
      if (config.web.observer) {
        log.info("  the interface is an observer and cannot change anything");
      }
    } catch (error) {
      web = null;
      log.error(String(error));
      log.warn("continuing without the web interface");
    }
  }

  const signals: Deno.Signal[] = ["SIGINT", "SIGTERM"];
  const onSignal = (signal: Deno.Signal) => () => stop(signal);
  const listeners = signals.map((signal) => {
    const handler = onSignal(signal);
    Deno.addSignalListener(signal, handler);
    return { signal, handler };
  });

  log.info("accepting messages", { channel: config.chat.channelId });

  const reason = await stopped;

  log.info("shutting down", { reason });
  if (statusTimer !== undefined) clearInterval(statusTimer);
  broker?.close();
  for (const { signal, handler } of listeners) Deno.removeSignalListener(signal, handler);
  await web?.stop();
  await daemon.shutdown();
  await gateway.close();
  releaseLock();
  return 0;
}
