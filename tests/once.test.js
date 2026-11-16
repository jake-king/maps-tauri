import { test } from "node:test";
import assert from "node:assert/strict";
import { once } from "../src/rust-renderer/once.js";

const deferred = () => {
  let resolve, reject;
  const promise = new Promise((res, rej) => ((resolve = res), (reject = rej)));
  return { promise, resolve, reject };
};

test("once: callers that arrive while the load is in flight share it", async () => {
  let calls = 0;
  const d = deferred();
  const load = once(() => (calls++, d.promise));
  const a = load();
  const b = load(); // arrives mid-flight
  d.resolve("ready");
  assert.deepEqual(await Promise.all([a, b]), ["ready", "ready"]);
  await load();
  assert.equal(calls, 1);
});

test("once: a failed load is not kept; the next call tries again", async () => {
  let calls = 0;
  const load = once(async () => {
    calls++;
    if (calls === 1) throw new Error("fetch failed");
    return "ready";
  });
  await assert.rejects(load(), /fetch failed/);
  assert.equal(await load(), "ready");
  assert.equal(calls, 2);
});

test("once: every caller of a failed flight sees the failure, and only one retry follows", async () => {
  let calls = 0;
  const d = deferred();
  const load = once(() => (calls++, calls === 1 ? d.promise : Promise.resolve("ready")));
  const a = load();
  const b = load();
  d.reject(new Error("boom"));
  await assert.rejects(a, /boom/);
  await assert.rejects(b, /boom/);
  const [c, e] = [load(), load()];
  assert.deepEqual(await Promise.all([c, e]), ["ready", "ready"]);
  assert.equal(calls, 2);
});

test("once: a function that throws synchronously is retried too", async () => {
  let calls = 0;
  const load = once(() => {
    calls++;
    if (calls === 1) throw new Error("sync");
    return Promise.resolve(7);
  });
  await assert.rejects(load(), /sync/);
  assert.equal(await load(), 7);
});
