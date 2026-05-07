import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { getConfig, setLibraryDir } from "@/lib/api";

const CONFIG_KEY = ["config"] as const;

export function useLibraryConfig() {
  return useQuery({
    queryKey: CONFIG_KEY,
    queryFn: getConfig,
    staleTime: Infinity,         // changes only via the mutation below
  });
}

export function useSetLibraryDir() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: setLibraryDir,
    onSuccess: (data) => {
      qc.setQueryData(CONFIG_KEY, data);
      // Albums + scan state will be re-fetched by Library on next mount.
      qc.invalidateQueries({ queryKey: ["albums"] });
    },
  });
}
