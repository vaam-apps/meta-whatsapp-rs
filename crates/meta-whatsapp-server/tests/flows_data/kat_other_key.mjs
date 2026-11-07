// Known-answer vectors for the service's Flows data endpoint, for a second key
// (roadmap M5d2). Derived from `kat_endpoint.mjs`.
//
// The WhatsApp client's side (wrap a random AES key with the business public
// key, encrypt the payload) is simulated; the business side is
// `decryptRequest` / `encryptResponse` from Meta's own Node.js example
// (flows/guides/implementingyourflowendpoint), which also confirm each
// request decrypts and compute the sealed answers the service must produce
// byte for byte (AES-GCM under a fixed key and IV is deterministic).
//
// The key is TEST-ONLY, never registered with Meta: a second one, beside this
// file (`test_rsa2048_other_pkcs8.pem`), for the per-tenant Flow key tests
// (roadmap M5d2): a request only that key decrypts. Run from this directory:
// `node kat_other_key.mjs > kat_other_key.json`.
import crypto from "crypto";
import fs from "fs";

const PRIVATE_KEY = fs.readFileSync("test_rsa2048_other_pkcs8.pem", "utf8");
const PUBLIC_KEY = fs.readFileSync("test_rsa2048_other_public.pem", "utf8");

const decryptRequest = (body, privatePem) => {
  const { encrypted_aes_key, encrypted_flow_data, initial_vector } = body;
  const decryptedAesKey = crypto.privateDecrypt(
    { key: crypto.createPrivateKey(privatePem), padding: crypto.constants.RSA_PKCS1_OAEP_PADDING, oaepHash: "sha256" },
    Buffer.from(encrypted_aes_key, "base64"),
  );
  const flowDataBuffer = Buffer.from(encrypted_flow_data, "base64");
  const initialVectorBuffer = Buffer.from(initial_vector, "base64");
  const TAG_LENGTH = 16;
  const decipher = crypto.createDecipheriv("aes-128-gcm", decryptedAesKey, initialVectorBuffer);
  decipher.setAuthTag(flowDataBuffer.subarray(-TAG_LENGTH));
  const text = Buffer.concat([decipher.update(flowDataBuffer.subarray(0, -TAG_LENGTH)), decipher.final()]).toString("utf-8");
  return { decryptedBody: JSON.parse(text), aesKeyBuffer: decryptedAesKey, initialVectorBuffer };
};

const encryptResponse = (response, aesKeyBuffer, initialVectorBuffer) => {
  const flipped_iv = [];
  for (const pair of initialVectorBuffer.entries()) flipped_iv.push(~pair[1]);
  const cipher = crypto.createCipheriv("aes-128-gcm", aesKeyBuffer, Buffer.from(flipped_iv));
  return Buffer.concat([cipher.update(JSON.stringify(response), "utf-8"), cipher.final(), cipher.getAuthTag()]).toString("base64");
};

const client = (payload) => {
  const requestJson = JSON.stringify(payload);
  const aesKey = crypto.randomBytes(16);
  const iv = crypto.randomBytes(16);
  const c = crypto.createCipheriv("aes-128-gcm", aesKey, iv);
  const data = Buffer.concat([c.update(requestJson, "utf-8"), c.final(), c.getAuthTag()]);
  const wrapped = crypto.publicEncrypt(
    { key: PUBLIC_KEY, padding: crypto.constants.RSA_PKCS1_OAEP_PADDING, oaepHash: "sha256" },
    aesKey,
  );
  return {
    request: {
      encrypted_flow_data: data.toString("base64"),
      encrypted_aes_key: wrapped.toString("base64"),
      initial_vector: iv.toString("base64"),
    },
    request_plaintext: requestJson,
  };
};

// Meta's payloads (flows/guides/implementingyourflowendpoint).
const scenarios = {
  // "Health check request".
  ping: [{ version: "3.0", action: "ping" }, { data: { status: "active" } }],
  // "Error notification request" and its acknowledgement.
  error_notification: [
    { version: "3.0", flow_token: "flowtoken-1234", action: "data_exchange", screen: "APPOINTMENT",
      data: { error: "invalid-screen-transition", error_message: "The response was invalid" } },
    { data: { acknowledged: true } },
  ],
  // "Data exchange request" and the "Next Screen Response Payload".
  data_exchange: [
    { version: "3.0", action: "data_exchange", screen: "APPOINTMENT",
      data: { department: "shopping", is_new_customer: true, guests: 2 }, flow_token: "flowtoken-1234" },
    { screen: "SUMMARY", data: { appointment: "Tuesday 10:00" } },
  ],
};

const out = {};
for (const [name, [payload, response]] of Object.entries(scenarios)) {
  const { request, request_plaintext } = client(payload);
  const { decryptedBody, aesKeyBuffer, initialVectorBuffer } = decryptRequest(request, PRIVATE_KEY);
  if (JSON.stringify(decryptedBody) !== request_plaintext) throw new Error("round trip failed: " + name);
  out[name] = {
    request,
    request_plaintext,
    response_plaintext: JSON.stringify(response),
    sealed_response: encryptResponse(response, aesKeyBuffer, initialVectorBuffer),
  };
}
console.log(JSON.stringify(out, null, 2));
