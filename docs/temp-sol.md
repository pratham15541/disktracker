 Why is size_delta always 0 in mutation_log?
The size_delta column in the mutation_log table is indeed always recorded as 0 by the Watcher.

The Reason: NTFS USN change journal records (the raw events we read from the Windows kernel) do not contain file sizes. They only contain the file's ID, name, attributes, and change reasons (e.g. data overwrite, rename, deletion).
Performance Choice: To calculate the change in size (delta) at the exact moment the Watcher is streaming events, the Watcher would have to open a handle to the filesystem for every single raw event. During high disk activity (like a build or file copy), this would heavily bottleneck the Watcher thread and drop events.
The Solution: Rather than storing deltas in mutation_log, we query the absolute size during the replay phase.