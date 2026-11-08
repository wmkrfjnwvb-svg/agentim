---
title: "Files and images"
description: "Uploads straight to S3 or through the server, thumbnails made on the device, location data removed, and links that check permission every time."
sidebar:
  order: 9
---

## Sending

From a client SDK, one call:

```ts
await client.sendFile(convId, file, { caption: 'The signed contract' });   // browser: a File
```

```swift
try await client.sendFile(convId, path: url.path, caption: "The signed contract")   // Swift, Kotlin, C#, Dart, React Native: a path
```

The SDK does the rest:

1. **Prepares the file on the device.** For images it removes EXIF and XMP metadata (including GPS) without re-encoding, makes an upright thumbnail of at most 320 px (JPEG, or PNG when transparent) and records the upright width and height.
2. **Shows it at once**, with the local preview and an upload progress bar (`status: uploading`).
3. **Uploads it in parts**, at most 4 at a time, resuming after network changes and restarts. With S3 the parts go straight to the bucket; the server never sees the bytes.
4. **Sends the message** of kind `image` or `file`, once the server has checked the upload.

Uploading doesn't hold the conversation back: messages typed during the upload go out, and the file takes its place in the order when it is done.

Agents and backends can send a whole file in one request instead: `sendFile` in the [agent SDKs](/docs/agent-sdks/), `send_file` over MCP (up to 16 MiB), or `POST /v1/conversations/{id}/files` from your backend (up to 64 MiB). The server removes the metadata and makes the thumbnail there.

## What the server checks

- The sender may post in the conversation: removed members, blocked direct conversations, dissolved conversations and agents in draft mode are refused like messages.
- The size is within `files.max_bytes` (4 GiB by default) and not zero.
- The real type, from the first bytes; the extension and the declared type are not trusted. JPEG, PNG, WebP and GIF are images; everything else, SVG included, is a file.
- The thumbnail is a JPEG or PNG of at most 320 px.
- A file belongs to exactly one message. Another message using it gets `conflict`.

## Downloading

`fileUrl(convId, fileId, 'thumb' | 'original')` returns a link valid for 5 minutes. Permission is checked each time: the uploader, or someone who can see a message with the file. Removed members lose access at once. With S3 it is a presigned link and the bytes don't pass through the server. Files never change, so apps cache them by `file_id`.

Plain files download with `Content-Disposition: attachment`; served locally they also get `nosniff` and a sandbox policy, so a file can never run as a page.

The UI components show images as thumbnails, open a viewer with **View original**, and show other files as a card with a type icon, name, size and download button.

## Lifecycle

| | |
|---|---|
| Unfinished uploads | Deleted after 24 hours |
| Uploaded but never sent | Deleted after 24 hours |
| Recalled message, rejected draft, dissolved conversation, deleted account | The files go with them |
| Retention | `files.retention_days` (default: kept). An expired file shows "File expired"; the message stays |
| Agent uploads | Marked `ai_generated: true` |

## Configuration

```toml
[files]
storage = "s3"            # or "local" (default), with dir = "./data/files"
max_bytes = 4294967296    # 4 GiB

[files.s3]
bucket = "agentim-files"
region = "us-east-1"
# endpoint = "https://…"  # MinIO, Cloudflare R2 and other S3-compatible stores
```

Credentials come from `AGENTIM_S3_ACCESS_KEY_ID` and `AGENTIM_S3_SECRET_ACCESS_KEY`. The bucket needs CORS for browser uploads ([file storage](/docs/operations/file-storage/)). A cluster needs S3, or a local folder shared by every node (`shared = true`).
