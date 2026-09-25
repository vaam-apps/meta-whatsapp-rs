// Known-answer vector for the Flows data-endpoint crypto.
// Client side (what the WhatsApp client does) is simulated; the business side
// uses decryptRequest/encryptResponse copied from Meta's docs
// (flows/guides/implementingyourflowendpoint, "Node.js Express example").
import crypto from "crypto";
import fs from "fs";

const PRIVATE_KEY = fs.readFileSync("test_rsa2048_pkcs8.pem", "utf8");
const PUBLIC_KEY = fs.readFileSync("test_rsa2048_public.pem", "utf8");

// ---- WhatsApp client side (simulated) ----
const requestJson = JSON.stringify({
  version: "3.0",
  action: "data_exchange",
  screen: "APPOINTMENT",
  data: { department: "shopping", is_new_customer: true, guests: 2 },
  flow_token: "flowtoken-1234",
});
const aesKey = crypto.randomBytes(16);
const iv = crypto.randomBytes(16);
const c = crypto.createCipheriv("aes-128-gcm", aesKey, iv);
const encryptedFlowData = Buffer.concat([c.update(requestJson, "utf-8"), c.final(), c.getAuthTag()]);
const encryptedAesKey = crypto.publicEncrypt(
  { key: PUBLIC_KEY, padding: crypto.constants.RSA_PKCS1_OAEP_PADDING, oaepHash: "sha256" },
  aesKey,
);
const body = {
  encrypted_flow_data: encryptedFlowData.toString("base64"),
  encrypted_aes_key: encryptedAesKey.toString("base64"),
  initial_vector: iv.toString("base64"),
};

// ---- Meta's documented business-side code (verbatim logic) ----
const decryptRequest = (body, privatePem) => {
  const { encrypted_aes_key, encrypted_flow_data, initial_vector } = body;
  const decryptedAesKey = crypto.privateDecrypt(
    { key: crypto.createPrivateKey(privatePem), padding: crypto.constants.RSA_PKCS1_OAEP_PADDING, oaepHash: "sha256" },
    Buffer.from(encrypted_aes_key, "base64"),
  );
  const flowDataBuffer = Buffer.from(encrypted_flow_data, "base64");
  const initialVectorBuffer = Buffer.from(initial_vector, "base64");
  const TAG_LENGTH = 16;
  const encrypted_flow_data_body = flowDataBuffer.subarray(0, -TAG_LENGTH);
  const encrypted_flow_data_tag = flowDataBuffer.subarray(-TAG_LENGTH);
  const decipher = crypto.createDecipheriv("aes-128-gcm", decryptedAesKey, initialVectorBuffer);
  decipher.setAuthTag(encrypted_flow_data_tag);
  const decryptedJSONString = Buffer.concat([decipher.update(encrypted_flow_data_body), decipher.final()]).toString("utf-8");
  return { decryptedBody: JSON.parse(decryptedJSONString), aesKeyBuffer: decryptedAesKey, initialVectorBuffer };
};

const encryptResponse = (response, aesKeyBuffer, initialVectorBuffer) => {
  const flipped_iv = [];
  for (const pair of initialVectorBuffer.entries()) {
    flipped_iv.push(~pair[1]);
  }
  const cipher = crypto.createCipheriv("aes-128-gcm", aesKeyBuffer, Buffer.from(flipped_iv));
  return Buffer.concat([cipher.update(JSON.stringify(response), "utf-8"), cipher.final(), cipher.getAuthTag()]).toString("base64");
};

const { decryptedBody, aesKeyBuffer, initialVectorBuffer } = decryptRequest(body, PRIVATE_KEY);
if (JSON.stringify(decryptedBody) !== requestJson) throw new Error("round trip failed");
if (!aesKeyBuffer.equals(aesKey)) throw new Error("key mismatch");

const health = { data: { status: "active" } };
const next = { screen: "SUCCESS", data: { extension_message_response: { params: { flow_token: "flowtoken-1234" } } } };

console.log(JSON.stringify({
  request: body,
  request_plaintext: requestJson,
  sealed_health_check: encryptResponse(health, aesKeyBuffer, initialVectorBuffer),
  health_plaintext: JSON.stringify(health),
  sealed_success: encryptResponse(next, aesKeyBuffer, initialVectorBuffer),
  success_plaintext: JSON.stringify(next),
}, null, 2));
