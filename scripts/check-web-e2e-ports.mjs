#!/usr/bin/env node

import { createServer } from "node:net";
import { writeSync } from "node:fs";

const HOST = "127.0.0.1";
const DEFAULT_PORTS = [
  { port: 3101, role: "fixture" },
  { port: 5174, role: "preview" },
];

const usage = () => {
  writeSync(
    process.stderr.fd,
    "usage: check-web-e2e-ports.mjs [--fixture-port PORT] [--preview-port PORT]\n",
  );
  process.exit(2);
};

const parsePort = (value) => {
  if (!/^[1-9][0-9]{0,4}$/.test(value)) usage();
  const port = Number(value);
  if (port > 65535) usage();
  return port;
};

const parsePorts = (arguments_) => {
  if (arguments_.length === 0) return DEFAULT_PORTS;
  const ports = [];
  for (let index = 0; index < arguments_.length; index += 2) {
    const flag = arguments_[index];
    const value = arguments_[index + 1];
    if (value === undefined) usage();
    if (flag === "--fixture-port") {
      ports.push({ port: parsePort(value), role: "fixture" });
    } else if (flag === "--preview-port") {
      ports.push({ port: parsePort(value), role: "preview" });
    } else {
      usage();
    }
  }
  return ports;
};

const occupiedPortMessage = ({ port, role }) =>
  role === "fixture"
    ? [
        `web E2E preflight failed: fixture API port ${HOST}:${port} is already in use.`,
        "Stop `just web-fixtures` or the other Atlas process using that port, then rerun `just test-web-e2e`.",
        "Playwright will not reuse an existing fixture server.",
      ].join("\n")
    : [
        `web E2E preflight failed: preview port ${HOST}:${port} is already in use.`,
        "Stop the process using that port, then rerun `just test-web-e2e`.",
        "Playwright will not reuse an existing preview server.",
      ].join("\n");

const checkPort = (entry) =>
  new Promise((resolve, reject) => {
    const server = createServer();
    server.unref();
    server.once("error", (error) => {
      if (error.code === "EADDRINUSE") {
        reject(new Error(occupiedPortMessage(entry)));
        return;
      }
      reject(
        new Error(
          `web E2E preflight could not inspect ${HOST}:${entry.port}: ${error.message}`,
        ),
      );
    });
    server.listen({ host: HOST, port: entry.port, exclusive: true }, () => {
      server.close((error) => {
        if (error === undefined) resolve();
        else reject(error);
      });
    });
  });

try {
  for (const entry of parsePorts(process.argv.slice(2))) {
    await checkPort(entry);
  }
} catch (error) {
  writeSync(
    process.stderr.fd,
    `${error instanceof Error ? error.message : String(error)}\n`,
  );
  process.exit(1);
}
