"""Pelagos Blog API — a simple note-taking REST API backed by Redis."""

import json
import os

from bottle import Bottle, request, response

import redis

app = Bottle()

REDIS_HOST = os.environ.get("REDIS_HOST", "redis")
REDIS_PORT = int(os.environ.get("REDIS_PORT", "6379"))
NOTES_KEY = "blog:notes"

def get_redis():
    return redis.Redis(host=REDIS_HOST, port=REDIS_PORT, decode_responses=True)


@app.route("/health")
def health():
    response.content_type = "application/json"
    return json.dumps({"status": "ok"})


@app.route("/api/notes", method="GET")
def list_notes():
    r = get_redis()
    notes = r.lrange(NOTES_KEY, 0, -1)
    response.content_type = "application/json"
    return json.dumps(notes)


@app.route("/api/notes", method="POST")
def add_note():
    body = request.body.read().decode("utf-8")
    try:
        data = json.loads(body)
        text = data.get("text", "").strip()
    except (json.JSONDecodeError, AttributeError):
        text = body.strip()

    if not text:
        response.status = 400
        response.content_type = "application/json"
        return json.dumps({"error": "text is required"})

    r = get_redis()
    r.rpush(NOTES_KEY, text)
    response.content_type = "application/json"
    return json.dumps({"ok": True, "text": text})


@app.route("/api/notes/count", method="GET")
def count_notes():
    r = get_redis()
    count = r.llen(NOTES_KEY)
    response.content_type = "application/json"
    return json.dumps({"count": count})


if __name__ == "__main__":
    app.run(host="0.0.0.0", port=5000)
