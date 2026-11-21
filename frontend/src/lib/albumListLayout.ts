/**
 * Which columns the album list (`components/AlbumList.tsx`) shows, from the
 * width it is given. The list beside the album panel can have as little as
 * 320 px; the window's width says nothing about that. Album and artist
 * always show; narrower, the length goes first, then the track count, then
 * the two stack in one column, then the year goes too.
 */
export interface ListLayout {
  stacked: boolean;
  year: boolean;
  tracks: boolean;
  length: boolean;
}

export const FULL_LIST_LAYOUT: ListLayout = { stacked: false, year: true, tracks: true, length: true };

export function listLayout(width: number | null): ListLayout {
  if (width === null || width >= 640) return FULL_LIST_LAYOUT;
  if (width >= 540) return { stacked: false, year: true, tracks: true, length: false };
  if (width >= 460) return { stacked: false, year: true, tracks: false, length: false };
  if (width >= 380) return { stacked: true, year: true, tracks: false, length: false };
  return { stacked: true, year: false, tracks: false, length: false };
}
