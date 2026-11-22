# Uploading Files

File uploads work by first sending a file to the server and then using the ID provided.

File tags and limits are defined by your instance configuration (see Autumn / `Revolt.toml` feature limits). A published Zrzur OpenAPI/Scalar site is **not** available yet — do not use Stoat CDN Scalar URLs for Zrzur.

To upload a file, pick the desired tag then send a **POST** to `{endpoint}/{tag}` along with a `multipart/form-data` body with one field `file` that contains the file you wish to upload.

You must specify session/bot authentication token as with any other API route.

You will receive the following JSON response:

```json
{
  "id": "0"
}
```

You can use the ID wherever a file is required in the API.

Code sample in JavaScript using Fetch API:

```js
const body = new FormData();
body.append("file", file);

const data = await fetch(`${endpoint}/${tag}`, {
  method: "POST",
  body,
  headers: {
    "X-Session-Token": "...", // or X-Bot-Token
  },
}).then((res) => res.json());

// use data.id
```

## Differences from old Autumn

If you are migrating from old Autumn, the following key points are important:

- There are only two paths that serve a unique image, the preview version of it (if available) and the original image.
- You should not specify any query parameters on the preview route, it will serve the optimal size for the content type. The one query on the original route is `?format=m4a` for voice messages (below).
- Preview routes for banners, emojis, backgrounds, and attachments will redirect to the original file where the file is not an image or the image is animated.
- If you are currently using logic to replace the URL path to start/stop animations, you should use the following templates: (NB. this only applies to avatars and icons)
  - Non-animated file: `/{tag}/{file_id}`
  - Animated file: `/{tag}/{file_id}/{file_name}` or `/{tag}/{file_id}/original` (if name unavailable)

## Voice messages: the AAC copy

Voice messages are attachments named `voice-message-<digits>.<ext>` (`ogg`, `webm`, `m4a` or `mp4`). Browsers record them as Opus in Ogg or WebM, which Safari plays only from 18.4 (Ogg) or 17.4 (WebM). For every voice message the server also keeps an AAC copy in MP4 that every browser plays:

- `GET {endpoint}/attachments/{file_id}/{file_name}?format=m4a` serves the copy as `audio/mp4`, with `Range` support like the original. `/original?format=m4a` redirects to the file name and keeps the query.
- The copy is made right after the upload; for older files, on the first request (that request waits for it). A recording that is AAC already is served as it is.
- Only voice-message attachments have a copy: on any other file, and for any other `format` value, the answer is 404.
- The copy goes when the file is deleted.

Ask for the copy only where the browser can't play the recording, for example when `audio.canPlayType('audio/ogg; codecs="opus"')` is empty for an Ogg file. Ogg Opus originals are served as `audio/ogg; codecs=opus`.
