import assert from "node:assert/strict";
import test from "node:test";
import {
  isSensitiveTransferPath,
  redactSensitiveText,
} from "../src/lib/externalTransferSafety.ts";

test("sensitive file names and parent folders are excluded from transfer scope", () => {
  for (const path of [
    ".env.production",
    "config/secrets/application.yml",
    "home/id_ed25519",
    "certs/service.pem",
    "settings/api-key.json",
  ]) {
    assert.equal(isSensitiveTransferPath(path), true, path);
  }
  assert.equal(isSensitiveTransferPath("src/main/App.kt"), false);
});

test("credential assignments, private keys, bearer values, and known tokens are redacted", () => {
  const input = [
    "api_key = sk-proj-abcdefghijklmnop1234",
    "OPENAI_API_KEY=openai-value",
    "AWS_SECRET_ACCESS_KEY=aws-value",
    "export THIRD_PARTY_TOKEN=token-value",
    "password: my-secret-password",
    "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9",
    "token=ghp_123456789012345678901234567890",
    "standalone: ghp_abcdefghijklmnopqrst1234567890",
    "-----BEGIN RSA PRIVATE KEY-----",
    "private-key-material",
    "-----END RSA PRIVATE KEY-----",
  ].join("\n");

  const redacted = redactSensitiveText(input);
  assert.doesNotMatch(redacted, /sk-proj-|openai-value|aws-value|token-value|my-secret-password|eyJhbGci|ghp_(?:123456|abcdefghijklmnop)|private-key-material/);
  assert.match(redacted, /\[REDACTED PRIVATE KEY\]/);
  assert.match(redacted, /\[REDACTED TOKEN\]/);
});
