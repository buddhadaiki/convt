import { afterEach, expect, test } from "bun:test";
import { Convt, ConvtError, type Job } from "../src/index";

const originalFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = originalFetch;
});

const client = () => new Convt({ apiKey: "test", baseUrl: "https://api.test" });
const fakeFetch = (handler: (...args: Parameters<typeof fetch>) => ReturnType<typeof fetch>) =>
  Object.assign(handler, { preconnect: originalFetch.preconnect });

test("JSON API errors retain their code, message and HTTP status", async () => {
  globalThis.fetch = fakeFetch(async () =>
    Response.json(
      { error: { code: "limit_reached", message: "Your allowance has been reached." } },
      { status: 403 },
    ),
  );

  const error = await client()
    .status("job_test")
    .catch((error: unknown) => error);

  expect(error).toBeInstanceOf(ConvtError);
  expect(error).toMatchObject({
    code: "limit_reached",
    message: "Your allowance has been reached.",
    status: 403,
  });
});

test.each([415, 422])("text validation errors retain HTTP %i as ConvtError", async (status) => {
  globalThis.fetch = fakeFetch(
    async () =>
      new Response("Failed to deserialize the JSON body into the target type", {
        status,
        headers: { "content-type": "text/plain" },
      }),
  );

  const error = await client()
    .create({ input_format: "svg", target_format: "png", input_bytes: NaN })
    .catch((error: unknown) => error);

  expect(error).toBeInstanceOf(ConvtError);
  expect(error).toMatchObject({ code: "request_failed", status });
});

test("HTML proxy errors retain the status without exposing the page as a message", async () => {
  globalThis.fetch = fakeFetch(
    async () =>
      new Response("<html><body>Bad gateway from upstream</body></html>", {
        status: 502,
        headers: { "content-type": "text/html" },
      }),
  );

  const error = await client()
    .status("job_test")
    .catch((error: unknown) => error);

  expect(error).toBeInstanceOf(ConvtError);
  expect(error).toMatchObject({ code: "request_failed", message: "Request failed.", status: 502 });
});

test("malformed JSON error responses retain their HTTP status", async () => {
  globalThis.fetch = fakeFetch(
    async () =>
      new Response('{"error":', {
        status: 503,
        headers: { "content-type": "application/json" },
      }),
  );

  const error = await client()
    .status("job_test")
    .catch((error: unknown) => error);

  expect(error).toBeInstanceOf(ConvtError);
  expect(error).toMatchObject({ code: "request_failed", status: 503 });
});

test("JSON error responses without an error object still preserve their status", async () => {
  globalThis.fetch = fakeFetch(async () => Response.json(null, { status: 500 }));

  const error = await client()
    .status("job_test")
    .catch((error: unknown) => error);

  expect(error).toBeInstanceOf(ConvtError);
  expect(error).toMatchObject({ code: "request_failed", message: "Request failed.", status: 500 });
});

test("malformed JSON on a successful response fails explicitly", async () => {
  globalThis.fetch = fakeFetch(
    async () =>
      new Response('{"job":', {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
  );

  const error = await client()
    .status("job_test")
    .catch((error: unknown) => error);

  expect(error).toBeInstanceOf(ConvtError);
  expect(error).toMatchObject({
    code: "invalid_response",
    message: "API returned an invalid JSON response.",
    status: 200,
  });
});

test("successful JSON responses still return the API payload", async () => {
  const job: Job = {
    id: "job_test",
    status: "queued",
    input_format: "svg",
    target_format: "png",
    input_bytes: 6,
    attempt: 0,
    error_code: null,
    expires_at: "2099-01-01T00:00:00Z",
  };
  globalThis.fetch = fakeFetch(async () => Response.json(job));

  expect(await client().status("job_test")).toEqual(job);
});

test("network failures remain the original fetch error", async () => {
  const failure = new TypeError("network connection failed");
  globalThis.fetch = fakeFetch(async () => {
    throw failure;
  });

  await expect(client().status("job_test")).rejects.toBe(failure);
});

for (const [kind, failure] of [
  ["cancellation", new DOMException("response body cancelled", "AbortError")],
  ["network failure", new TypeError("response body connection failed")],
] as const) {
  test(`response body ${kind} preserves the original error`, async () => {
    const body = new ReadableStream({
      start(controller) {
        controller.error(failure);
      },
    });
    globalThis.fetch = fakeFetch(async () => new Response(body, { status: 502 }));

    await expect(client().status("job_test")).rejects.toBe(failure);
  });
}
