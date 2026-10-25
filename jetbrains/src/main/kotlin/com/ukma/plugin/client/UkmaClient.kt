package com.ukma.plugin.client

import org.json.JSONObject
import java.net.URI
import java.net.http.HttpClient
import java.net.http.HttpRequest
import java.net.http.HttpResponse
import java.time.Duration

/**
 * Thin REST client over the UKMA server (scaffold).
 *
 * The plugin is a *client*: it carries no business logic. It lists tools and
 * calls them against the UKMA server's REST surface (`/tools`, `/call`), and
 * the finished plugin renders the results in the tool window.
 */
class UkmaClient(private val baseUrl: String = "http://127.0.0.1:8080") {
    private val http = HttpClient.newBuilder()
        .connectTimeout(java.time.Duration.ofSeconds(5))
        .build()

    /** Whether the server is reachable (GET /health). */
    fun ping(): Boolean {
        val (status, _) = request("GET", "/health", null) ?: return false
        return status in 200..299
    }

    /** The registered tool names (system surface). */
    fun toolNames(): List<String> {
        val (_, body) = request("GET", "/tools", null) ?: return emptyList()
        return parseToolNames(body)
    }

    /** Call one operation with a JSON argument object; the response body. */
    fun call(operation: String, args: JSONObject): String? {
        val payload = JSONObject().put("operation", operation).put("args", args).toString()
        val (status, body) = request("POST", "/call", payload) ?: return null
        return if (status in 200..299) body else null
    }

    private fun parseToolNames(body: String): List<String> =
        runCatching {
            val root = JSONObject(body)
            val arr = root.optJSONArray("tools")
            (0 until (arr?.length() ?: 0)).map { arr!!.get(it).toString() }
        }.getOrDefault(emptyList())

    /** Send a request; returns (status, body) or null on transport failure. */
    private fun request(method: String, path: String, body: String?): Pair<Int, String>? =
        runCatching {
            val builder = HttpRequest.newBuilder(URI.create(baseUrl + path))
                .timeout(Duration.ofSeconds(10))
                .header("Content-Type", "application/json")
                .method(
                    method,
                    if (body != null) HttpRequest.BodyPublishers.ofString(body)
                    else HttpRequest.BodyPublishers.noBody()
                )
            val response = http.send(builder.build(), HttpResponse.BodyHandlers.ofString())
            response.statusCode() to response.body()
        }.getOrNull()
}
