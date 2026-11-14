import { useAiProviders } from "./useJobs";
import { aiStatus, type AiStatus } from "@/lib/aiGating";

/** Whether a model backend can run AI jobs (see lib/aiGating.ts). */
export function useAiStatus(): AiStatus {
  const { data } = useAiProviders();
  return aiStatus(data);
}
