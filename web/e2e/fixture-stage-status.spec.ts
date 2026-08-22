import { expect, test } from "@playwright/test";

interface SourceList {
  sources: Array<{ source_id: string }>;
}

interface StageDescriptor {
  classifier_id?: string;
  content_id: string;
  kind: "population" | "membership" | "structure" | "classifier";
}

interface SnapshotManifest {
  stages: StageDescriptor[];
}

test("fixture stage failures preserve the v2 retry boundary", async ({
  request,
}) => {
  const sourcesResponse = await request.get("/api/v2/sources");
  expect(sourcesResponse.status()).toBe(200);
  const sources = (await sourcesResponse.json()) as SourceList;
  const sourceId = sources.sources[0]?.source_id;
  if (sourceId === undefined)
    throw new Error("Fixture API published no source");

  const manifestResponse = await request.get(
    `/api/v2/sources/${encodeURIComponent(sourceId)}/mempool`,
  );
  expect(manifestResponse.status()).toBe(200);
  const manifest = (await manifestResponse.json()) as SnapshotManifest;
  const population = manifest.stages.find(({ kind }) => kind === "population");
  const membership = manifest.stages.find(({ kind }) => kind === "membership");
  if (population === undefined || membership === undefined) {
    throw new Error("Fixture manifest omitted a required stage");
  }

  const currentIds = new Set(
    manifest.stages.map(({ content_id }) => content_id),
  );
  const supersededId = ["0".repeat(64), "f".repeat(64)].find(
    (candidate) => !currentIds.has(candidate),
  );
  if (supersededId === undefined) {
    throw new Error("Fixture exhausted the superseded test IDs");
  }
  const base = `/api/v2/sources/${encodeURIComponent(sourceId)}/mempool/stages`;

  for (const [path, expectedStatus] of [
    [`${base}/population/not-a-digest`, 400],
    [`${base}/unknown/${supersededId}`, 400],
    [`${base}/classifier/Unknown/${supersededId}`, 400],
    [`${base}/classifier/unknown/${supersededId}`, 404],
    [`${base}/membership/${population.content_id}`, 404],
    [
      `/api/v2/sources/unknown-source/mempool/stages/population/${supersededId}`,
      404,
    ],
    [`${base}/population/${supersededId}`, 409],
  ] as const) {
    const response = await request.get(path);
    expect(response.status(), path).toBe(expectedStatus);
    expect(response.headers()["cache-control"], path).toBe("no-store");
  }
});
