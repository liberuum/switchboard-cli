// Regenerates tests/fixtures/v2_vectors.json from the reactor's own v2 hash.
//
//   node tests/fixtures/gen_v2_vectors.mjs <path to @powerhousedao/shared>
//
// e.g. .../bai-knowledge-note/node_modules/@powerhousedao/shared
//
// Every `input` is JSON *text*, parsed by JSON.parse here and by
// serde_json in the Rust test: the same bytes the CLI reads from the user,
// and the server reads off the wire, so both sides start from the same value.
import { readFileSync, writeFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const sharedDir = process.argv[2];
if (!sharedDir) throw new Error("usage: gen_v2_vectors.mjs <path to @powerhousedao/shared>");
const { version } = JSON.parse(readFileSync(join(sharedDir, "package.json"), "utf8"));
const dm = await import(pathToFileURL(join(sharedDir, "dist/document-model/index.js")).href);

const signer = {
  user: { address: "0xadbA7C2F82139031D7564D18aC22D09B12A0BcA4", networkId: "eip155", chainId: 1 },
  app: { name: "switchboard-cli", key: "did:key:zDnaevybtcoaSAYQtPqgiiJFxdJUx1m9ErTeD8a7qwGxxBSxU" },
};
const JOB_DOC = "1LGfE6gA0RDQ2urs7l9BxMXpCcpVN8irrcZlLddqCkI";

const cases = [
  ["simple", "SET_TITLE", "global", `{"title":"Hello","updatedAt":"2026-09-25T10:00:00.000Z"}`],
  ["unsorted_nested", "SET_METADATA", "global", `{"zeta":1,"alpha":{"b":2,"a":1},"mid":[{"y":1,"x":2}]}`],
  ["numbers", "SET_STATS", "global", `{"int":42,"neg":-7,"frac":0.1,"integralFloat":2.0,"big":1e21,"small":1e-7,"zero":0,"negZero":-0.0,"huge":1.5e300,"half":0.5,"third":0.3333333333333333}`],
  // Every branch of Number::toString: the 1e21 / 1e-6 thresholds on both
  // sides, a point inside the digits, negatives, and an integer past 2^53
  // (JavaScript holds 9007199254740993 as 9007199254740992).
  ["number_boundaries", "SET_STATS", "global", `{"e20":1e20,"e21":1e21,"m6":1e-6,"m7":1e-7,"mid":123.456,"negFrac":-2.5,"negSmall":-1e-7,"pastSafe":9007199254740993,"hundred":100,"tiny":1.23e-5,"negBig":-4.5e22}`],
  ["unicode", "SET_TITLE", "global", `{"title":"em—dash ☃ 😀 中文 é"}`],
  ["escapes", "SET_CONTENT", "global", `{"content":"quote \\" backslash \\\\ newline \\n tab \\t ctrl \\u0001 slash / del \\u007f"}`],
  ["literals", "SET_FLAGS", "global", `{"n":null,"t":true,"f":false,"emptyArr":[],"emptyObj":{}}`],
  ["empty_input", "NOOP_LIKE", "global", `{}`],
  // U+FF01 sorts AFTER an astral char in UTF-16 (FF01 > D83D) but BEFORE it in
  // UTF-8 bytes (EF.. < F0..): the case where JavaScript's key order and a
  // naive Rust byte sort disagree.
  ["unicode_keys", "SET_METADATA", "global", `{"\uff01":1,"\ud83d\ude00":2,"z":3,"\u00e9":4,"a":5,"\u4e2d":6}`],
  ["relationship", "ADD_RELATIONSHIP", "document", `{"sourceId":"SRC-DOC-ID","targetId":"TGT-DOC-ID","relationshipType":"BUILDS_ON","metadata":{"reason":"why","confidence":"grounded"}}`],
  ["relationship_no_source", "REMOVE_RELATIONSHIP", "document", `{"targetId":"TGT-DOC-ID","relationshipType":"BUILDS_ON"}`],
  ["create_document", "CREATE_DOCUMENT", "document", `{"documentId":"NEW-DOC-ID","model":"bai/knowledge-note"}`],
  ["delete_document_no_id", "DELETE_DOCUMENT", "document", `{}`],
];

const vectors = [];
for (const [name, type, scope, inputText] of cases) {
  const action = {
    id: `action-${name}`,
    type,
    scope,
    timestampUtcMs: "2026-09-25T10:00:00.000Z",
    input: JSON.parse(inputText),
  };
  const target = dm.actionSigningTarget(action, JOB_DOC, "main");
  vectors.push({
    name,
    type,
    scope,
    id: action.id,
    timestampUtcMs: action.timestampUtcMs,
    inputText,
    jobDocumentId: JOB_DOC,
    branch: "main",
    expectedTargetDocumentId: target.documentId,
    expectedPreimage: dm.actionPreimageV2(action, target, signer),
    expectedHash: await dm.hashActionV2(action, target, signer),
  });
}

const out = { generatedFrom: `@powerhousedao/shared@${version}`, signer, vectors };
const path = join(dirname(fileURLToPath(import.meta.url)), "v2_vectors.json");
writeFileSync(path, JSON.stringify(out, null, 2) + "\n");
console.log(`wrote ${vectors.length} vectors from ${out.generatedFrom} -> ${path}`);
