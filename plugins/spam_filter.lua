-- Spam Filter Plugin
--
-- Filters messages based on content patterns, sender reputation,
-- and rate limiting rules.

plugin = {
    name = "spam_filter",
    version = "1.0.0"
}

-- Configuration
local config = {
    max_messages_per_minute = 100,
    max_message_length = 1600,
    blocked_keywords = {},
    suspicious_patterns = {},
    whitelist_senders = {},
    blacklist_senders = {}
}

-- Rate limiting state
local sender_message_counts = {}
local last_cleanup = os.time()
local CLEANUP_INTERVAL = 60

-- Initialize the plugin
function plugin.initialize(user_config)
    if user_config then
        if user_config.max_messages_per_minute then
            config.max_messages_per_minute = tonumber(user_config.max_messages_per_minute) or 100
        end
        if user_config.max_message_length then
            config.max_message_length = tonumber(user_config.max_message_length) or 1600
        end
        if user_config.blocked_keywords then
            config.blocked_keywords = user_config.blocked_keywords or {}
        end
        if user_config.suspicious_patterns then
            config.suspicious_patterns = user_config.suspicious_patterns or {}
        end
        if user_config.whitelist_senders then
            config.whitelist_senders = user_config.whitelist_senders or {}
        end
        if user_config.blacklist_senders then
            config.blacklist_senders = user_config.blacklist_senders or {}
        end
    end

    smppd.log("info", string.format(
        "spam_filter initialized: max_rate=%d/min, max_length=%d",
        config.max_messages_per_minute,
        config.max_message_length
    ))

    return true
end

-- Shutdown the plugin
function plugin.shutdown()
    smppd.log("info", "spam_filter shutting down")
end

-- Helper: Check if sender is whitelisted
local function is_whitelisted(sender)
    for _, s in ipairs(config.whitelist_senders) do
        if sender == s or sender:match(s) then
            return true
        end
    end
    return false
end

-- Helper: Check if sender is blacklisted
local function is_blacklisted(sender)
    for _, s in ipairs(config.blacklist_senders) do
        if sender == s or sender:match(s) then
            return true
        end
    end
    return false
end

-- Helper: Check content for blocked keywords
local function has_blocked_keywords(content)
    local lower_content = content:lower()
    for _, keyword in ipairs(config.blocked_keywords) do
        if lower_content:find(keyword:lower(), 1, true) then
            return true, keyword
        end
    end
    return false, nil
end

-- Helper: Check content for suspicious patterns
local function has_suspicious_patterns(content)
    for _, pattern in ipairs(config.suspicious_patterns) do
        if content:match(pattern) then
            return true, pattern
        end
    end
    return false, nil
end

-- Helper: Check rate limiting
local function is_rate_limited(system_id)
    local now = os.time()

    -- Cleanup old entries periodically
    if now - last_cleanup > CLEANUP_INTERVAL then
        sender_message_counts = {}
        last_cleanup = now
    end

    -- Get or initialize counter
    local counter = sender_message_counts[system_id]
    if not counter then
        counter = { count = 0, window_start = now }
        sender_message_counts[system_id] = counter
    end

    -- Reset if window expired
    if now - counter.window_start >= 60 then
        counter.count = 0
        counter.window_start = now
    end

    -- Check limit
    if counter.count >= config.max_messages_per_minute then
        return true
    end

    -- Increment counter
    counter.count = counter.count + 1
    return false
end

-- Main filter function
function plugin.filter(context)
    local sender = context.source or ""
    local system_id = context.system_id or ""
    local content = context.content or ""
    local destination = context.destination or ""

    smppd.log("debug", string.format(
        "Filtering message from %s to %s (system_id=%s)",
        sender, destination, system_id
    ))

    -- Check whitelist first
    if is_whitelisted(sender) then
        smppd.log("debug", string.format("Sender %s is whitelisted", sender))
        return { action = "continue" }
    end

    -- Check blacklist
    if is_blacklisted(sender) then
        smppd.log("warn", string.format("Sender %s is blacklisted", sender))
        return {
            action = "reject",
            status = 88,  -- ESME_RINVSRCADR (Invalid Source Address)
            reason = "Sender is blacklisted"
        }
    end

    -- Check message length
    if #content > config.max_message_length then
        smppd.log("warn", string.format(
            "Message too long: %d bytes (max %d)",
            #content, config.max_message_length
        ))
        return {
            action = "reject",
            status = 1,  -- ESME_RINVMSGLEN
            reason = "Message too long"
        }
    end

    -- Check rate limiting
    if is_rate_limited(system_id) then
        smppd.log("warn", string.format(
            "Rate limit exceeded for system_id=%s",
            system_id
        ))
        return {
            action = "reject",
            status = 88,  -- ESME_RTHROTTLED
            reason = "Rate limit exceeded"
        }
    end

    -- Check for blocked keywords
    local blocked, keyword = has_blocked_keywords(content)
    if blocked then
        smppd.log("warn", string.format(
            "Blocked keyword '%s' found in message",
            keyword
        ))
        return {
            action = "drop"
        }
    end

    -- Check for suspicious patterns
    local suspicious, pattern = has_suspicious_patterns(content)
    if suspicious then
        smppd.log("info", string.format(
            "Suspicious pattern detected, quarantining message"
        ))
        return {
            action = "quarantine"
        }
    end

    -- All checks passed
    return { action = "continue" }
end

return plugin
