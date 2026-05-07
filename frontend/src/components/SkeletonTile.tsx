/**
 * Loading placeholder shown in the grid while a fresh scan is producing
 * albums but they haven't arrived yet. Pulses gently.
 */
export default function SkeletonTile() {
  return (
    <div className="flex flex-col overflow-hidden rounded-md border border-line bg-panel">
      <div className="aspect-square bg-bg-2 animate-[skel-pulse_1.5s_ease-in-out_infinite]" />
      <div className="p-2.5 space-y-1.5 min-h-[64px]">
        <div className="h-3.5 bg-bg-2 rounded w-3/4 animate-[skel-pulse_1.5s_ease-in-out_infinite]" />
        <div className="h-3 bg-bg-2 rounded w-1/2 animate-[skel-pulse_1.5s_ease-in-out_infinite]" />
        <div className="h-2.5 bg-bg-2 rounded w-1/3 animate-[skel-pulse_1.5s_ease-in-out_infinite]" />
      </div>
    </div>
  );
}
