const sensitivePathSegment = /^(?:\.env(?:\..*)?|\.npmrc|\.pypirc|\.netrc|id_(?:rsa|ed25519|ecdsa|dsa)(?:\.pub)?|.*(?:secret|credential|password|token|private[-_]?key|api[-_]?key).*|.*\.(?:pem|key|p12|pfx|jks|keystore))$/i;

const secretAssignment = /^(\s*(?:export\s+)?["']?[A-Za-z0-9_.-]*(?:api[-_]?key|access[-_]?key|secret(?:[-_]?key)?|client[-_]?secret|password|passwd|token|private[-_]?key|authorization)[A-Za-z0-9_.-]*["']?\s*[:=]\s*).+$/i;
const privateKeyBlock = /-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z0-9 ]*PRIVATE KEY-----/g;
const bearerCredential = /\b(Bearer\s+)[A-Za-z0-9._~+/-]+=*/gi;
const knownToken = /\b(?:sk-[A-Za-z0-9_-]{16,}|gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|AKIA[0-9A-Z]{16}|AIza[0-9A-Za-z_-]{30,})\b/g;

export function isSensitiveTransferPath(path: string): boolean {
  return path.replace(/\\/g, "/").split("/").some((segment) => sensitivePathSegment.test(segment));
}

export function redactSensitiveText(text: string): string {
  return text
    .replace(privateKeyBlock, "[REDACTED PRIVATE KEY]")
    .replace(/^.*$/gm, (line) => secretAssignment.test(line) ? `${line.match(secretAssignment)?.[1] ?? ""}[REDACTED]` : line)
    .replace(bearerCredential, "$1[REDACTED]")
    .replace(knownToken, "[REDACTED TOKEN]");
}
