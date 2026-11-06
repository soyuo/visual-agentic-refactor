import { invoke } from "@tauri-apps/api/core";

export type CommandErrorCode = "invalidInput" | "cancelled" | "internal";

export interface CommandErrorPayload {
  code: CommandErrorCode;
  message: string;
}

export class AppCommandError extends Error {
  constructor(
    readonly code: CommandErrorCode,
    message: string,
  ) {
    super(message);
    this.name = "AppCommandError";
  }
}

export interface AppInfoResponse {
  appName: string;
  version: string;
}

const commandErrorCodes: readonly string[] = [
  "invalidInput",
  "cancelled",
  "internal",
];

function isCommandErrorPayload(value: unknown): value is CommandErrorPayload {
  if (typeof value !== "object" || value === null) return false;

  const payload = value as Record<string, unknown>;
  return (
    typeof payload.code === "string" &&
    commandErrorCodes.includes(payload.code) &&
    typeof payload.message === "string"
  );
}

export function toAppCommandError(error: unknown): AppCommandError {
  if (error instanceof AppCommandError) return error;
  if (isCommandErrorPayload(error)) {
    return new AppCommandError(error.code, error.message);
  }

  return new AppCommandError(
    "internal",
    "데스크톱 코어 명령을 처리하지 못했습니다.",
  );
}

export async function getAppInfo(): Promise<AppInfoResponse> {
  try {
    return await invoke<AppInfoResponse>("get_app_info");
  } catch (error) {
    throw toAppCommandError(error);
  }
}
