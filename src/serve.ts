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
import { assertChannelUsable, ChatThreadFactory, plain } from "./chat/threads.ts";
import { redactText, secretValues } from "./config/redact.ts";
import type { Config } from "./config/schema.ts";
import { createSandbox, Daemon, probeSandbox } from "./daemon.ts";
import { acquireLock } from "./lock.ts";
import type { Logger } from "./log.ts";
import { MemoryStore } from "./memory/store.ts";
import { agentDirectory } from "./provider/models.ts";
import { imageDescriber } from "./provider/vision.ts";
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
  // The sandbox is checked before the chat service is touched, so a missing
  // image or an unenforceable guarantee fails immediately rather than after a
  // login round trip.
  const sandbox = createSandbox(config, log);
  const report = await probeSandbox(sandbox, config, log);

  // The connection signals readiness during connect(), before the thread
  // factory and the daemon exist, so the handlers reach them through holders
  // rather than closing over bindings that are not initialised yet.
  let threads: ChatThreadFactory | null = null;
  let daemon: Daemon | null = null;
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

  // Decided once: what a model accepts is the provider's business, not
  // something to work out per attachment.
  const describer = imageDescriber(config.agent, agentDirectory(Deno.env.toObject()));
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
    ...(describer === undefined ? {} : { describeImages: describer.describe }),
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
  for (const { signal, handler } of listeners) Deno.removeSignalListener(signal, handler);
  await web?.stop();
  await daemon.shutdown();
  await gateway.close();
  releaseLock();
  return 0;
}
