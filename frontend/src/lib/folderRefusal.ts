/**
 * The server's reason for refusing a library folder, in plain words. The
 * server says "Not a directory: <path>" for a path that is missing or is a
 * file, and "Cannot read folder: <path> (<cause>)" for one it cannot list;
 * any other reason is kept as it is.
 */
export function folderRefusalText(message: string): string {
  const notDir = /^Not a directory: (.+)$/.exec(message);
  if (notDir) {
    return `Not saved. There is no folder at ${notDir[1]}. Check the spelling, or choose a folder, not a file.`;
  }
  const unreadable = /^Cannot read folder: (.+?) \(/.exec(message);
  if (unreadable) {
    return `Not saved. Musaic can't open ${unreadable[1]}. Check that it is connected and that you may read it.`;
  }
  return message ? `Not saved. ${message}` : "Not saved. Couldn't save the library folder.";
}
