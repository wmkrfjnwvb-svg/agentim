---
title: "File storage"
description: "A local folder or S3-compatible storage for files and images, with the bucket settings browsers need."
sidebar:
  order: 4
---

```toml
[files]
storage = "local"            # default
dir = "./data/files"
# shared = true              # every node of a cluster mounts the same folder
max_bytes = 4294967296       # 4 GiB per file
# daily_bytes_per_user = 0   # no limit
max_open_per_user = 10       # uploads in progress per person
# retention_days = 0         # files are kept
allow_metadata = false       # apps may not keep EXIF/GPS on photos
```

## Local folder

Uploads and downloads go through the server. Back the folder up with the database. Behind a proxy, allow 20 MB request bodies (16 MiB parts). Encrypt the disk (LUKS, BitLocker, cloud disk encryption): files in a local folder are not yet encrypted by AgentIM itself.

## S3 and compatible stores

```toml
[files]
storage = "s3"

[files.s3]
bucket = "agentim-files"
region = "us-east-1"
# endpoint = "https://<account>.r2.cloudflarestorage.com"   # R2, MinIO, other S3-compatible stores
# prefix = "prod/"
# path_style = false
```

Credentials come from `AGENTIM_S3_ACCESS_KEY_ID` and `AGENTIM_S3_SECRET_ACCESS_KEY`, never the file. At start the server writes and deletes a probe object; if it can't, it doesn't start.

Clients talk to the bucket directly, so set up three things:

1. **CORS**: allow your apps' origins to `PUT` and `GET`, allow the headers `content-type` and `content-length`, and **expose `ETag`**. Without it a browser can't finish a multipart upload.

   ```json
   [{
     "AllowedOrigins": ["https://app.example.com"],
     "AllowedMethods": ["PUT", "GET"],
     "AllowedHeaders": ["content-type", "content-length"],
     "ExposeHeaders": ["ETag"],
     "MaxAgeSeconds": 3600
   }]
   ```

2. **A lifecycle rule** that aborts incomplete multipart uploads after 1 day. The server cleans up too; this catches what it couldn't.
3. **Server-side encryption** (SSE-S3 or SSE-KMS).

Objects live at `{prefix}/{conv_id}/{file_id}/original` and `…/thumb`. File names stay in the database, not in object keys.
