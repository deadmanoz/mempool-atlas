import type {
  Bip110Assessment,
  ClassificationResultState,
  StagedSnapshotManifest,
} from "./types";
import type { AtlasProblem } from "./atlas-problem";

export interface PackedUnsignedColumnTransfer {
  width: number;
  values: ArrayBuffer;
}

export interface PackedSignedColumnTransfer {
  width: number;
  values: ArrayBuffer;
}

export interface PackedPopulationTransfer {
  contentId: string;
  txids: ArrayBuffer;
  vsize: PackedUnsignedColumnTransfer;
}

export interface PackedMembershipTransfer {
  contentId: string;
  differingWtxidBits: ArrayBuffer;
  differingWtxidRanks: ArrayBuffer;
  differingWtxids: ArrayBuffer;
  weight: PackedUnsignedColumnTransfer;
  feeSats: PackedUnsignedColumnTransfer;
  enteredAtMs: PackedUnsignedColumnTransfer;
  ancestorCount: PackedUnsignedColumnTransfer;
  ancestorVsize: PackedUnsignedColumnTransfer;
  ancestorFeeSats: PackedSignedColumnTransfer;
  descendantCount: PackedUnsignedColumnTransfer;
  descendantVsize: PackedUnsignedColumnTransfer;
  replaceableBits: ArrayBuffer;
}

export interface PackedStructureTransfer {
  contentId: string;
  presenceBits: ArrayBuffer;
  presenceRanks: ArrayBuffer;
  inputCount: PackedUnsignedColumnTransfer;
  outputCount: PackedUnsignedColumnTransfer;
  opReturnBytes: PackedUnsignedColumnTransfer;
  outputSats: PackedUnsignedColumnTransfer;
  witnessBytes: PackedUnsignedColumnTransfer;
}

export interface ClassifierResultTupleTransfer {
  state: ClassificationResultState;
  primary_label: string | null;
  labels: string[];
  missing_facts: string[];
}

export interface PackedClassifierTransfer {
  contentId: string;
  classifierId: string;
  resultDictionary: ClassifierResultTupleTransfer[];
  resultCodes: PackedUnsignedColumnTransfer;
  assessmentDictionary: Bip110Assessment[] | null;
  assessmentCodes: PackedUnsignedColumnTransfer | null;
}

export interface PackedPublicationTransfer {
  manifest: StagedSnapshotManifest;
  population: PackedPopulationTransfer;
  membership: PackedMembershipTransfer;
  structure: PackedStructureTransfer;
  classifiers: PackedClassifierTransfer[];
}

export interface PackedPrimaryPublicationTransfer {
  manifest: StagedSnapshotManifest;
  population: PackedPopulationTransfer;
  classifiers: PackedClassifierTransfer[];
}

export interface WorkerStageTiming {
  kind: "population" | "membership" | "structure" | "classifier";
  classifierId: string | null;
  reused: boolean;
  fetchDigestParseMs: number;
  decodeValidatePackMs: number;
}

export interface WorkerQuorumTiming {
  manifestFetchValidateMs: number;
  stages: WorkerStageTiming[];
  semanticValidationMs: number;
  quorumMs: number;
  supersessionRestarts: number;
  populationRebaseMs: number | null;
}

export interface WorkerLoadRequest {
  type: "load";
  requestId: number;
  sourceId: string;
  selectedClassifierId: string;
}

export interface WorkerCancelRequest {
  type: "cancel";
  requestId: number;
}

export type AtlasWorkerRequest = WorkerLoadRequest | WorkerCancelRequest;

export interface WorkerPrimaryResponse {
  type: "primary";
  requestId: number;
  publication: PackedPrimaryPublicationTransfer;
  timing: WorkerQuorumTiming;
}

export interface WorkerCompleteResponse {
  type: "complete";
  requestId: number;
  publication: PackedPublicationTransfer;
  timing: WorkerQuorumTiming;
}

export interface WorkerErrorResponse {
  type: "error";
  requestId: number;
  status: number | null;
  problem: AtlasProblem | null;
  message: string;
  retryable: boolean;
}

export type AtlasWorkerResponse =
  WorkerPrimaryResponse | WorkerCompleteResponse | WorkerErrorResponse;
