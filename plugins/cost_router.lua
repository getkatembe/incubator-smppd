-- Cost-based Route Selection Plugin
--
-- Selects routes based on cost, quality, and strategy.
-- Supports multiple routing strategies:
--   - least_cost: Always select the cheapest route
--   - quality_first: Prioritize quality over cost
--   - balanced: Balance between cost and quality
--   - round_robin: Rotate between routes

plugin = {
    name = "cost_router",
    version = "1.0.0"
}

-- Configuration
local config = {
    strategy = "balanced",
    quality_weight = 0.6,
    cost_weight = 0.4,
    latency_threshold_ms = 500,
    min_quality_score = 0.7
}

-- State
local route_index = 0

-- Initialize the plugin
function plugin.initialize(user_config)
    if user_config then
        if user_config.strategy then
            config.strategy = user_config.strategy
        end
        if user_config.quality_weight then
            config.quality_weight = tonumber(user_config.quality_weight) or 0.6
        end
        if user_config.cost_weight then
            config.cost_weight = tonumber(user_config.cost_weight) or 0.4
        end
        if user_config.latency_threshold_ms then
            config.latency_threshold_ms = tonumber(user_config.latency_threshold_ms) or 500
        end
        if user_config.min_quality_score then
            config.min_quality_score = tonumber(user_config.min_quality_score) or 0.7
        end
    end

    smppd.log("info", string.format(
        "cost_router initialized with strategy=%s, quality_weight=%.2f, cost_weight=%.2f",
        config.strategy, config.quality_weight, config.cost_weight
    ))

    return true
end

-- Shutdown the plugin
function plugin.shutdown()
    smppd.log("info", "cost_router shutting down")
end

-- Calculate score for a candidate route
local function calculate_score(candidate)
    local quality = candidate.quality_score or 0.5
    local cost = candidate.cost or 0
    local latency = candidate.latency_ms or 100

    -- Normalize cost (lower is better, invert to 0-1 scale)
    -- Assume max cost is 100 cents
    local cost_score = 1 - math.min(cost / 100, 1)

    -- Normalize latency (lower is better)
    local latency_score = 1 - math.min(latency / 1000, 1)

    -- Combined score
    local score = (config.quality_weight * quality) +
                  (config.cost_weight * cost_score) +
                  (0.1 * latency_score)

    return score
end

-- Filter candidates that don't meet minimum requirements
local function filter_viable(candidates)
    local viable = {}
    for i, c in ipairs(candidates) do
        local quality = c.quality_score or 0.5
        local latency = c.latency_ms or 100

        if quality >= config.min_quality_score and
           latency <= config.latency_threshold_ms then
            table.insert(viable, c)
        end
    end

    -- If no viable candidates, return all candidates
    if #viable == 0 then
        return candidates
    end

    return viable
end

-- Select route using least_cost strategy
local function select_least_cost(candidates)
    local best = nil
    local lowest_cost = math.huge

    for i, c in ipairs(candidates) do
        local cost = c.cost or 0
        if cost < lowest_cost then
            lowest_cost = cost
            best = c
        end
    end

    return best
end

-- Select route using quality_first strategy
local function select_quality_first(candidates)
    local best = nil
    local highest_quality = -1

    for i, c in ipairs(candidates) do
        local quality = c.quality_score or 0
        if quality > highest_quality then
            highest_quality = quality
            best = c
        end
    end

    return best
end

-- Select route using balanced strategy
local function select_balanced(candidates)
    local best = nil
    local best_score = -1

    for i, c in ipairs(candidates) do
        local score = calculate_score(c)
        if score > best_score then
            best_score = score
            best = c
        end
    end

    return best
end

-- Select route using round_robin strategy
local function select_round_robin(candidates)
    if #candidates == 0 then
        return nil
    end

    route_index = (route_index % #candidates) + 1
    return candidates[route_index]
end

-- Main route selection function
function plugin.select_route(context, candidates)
    if not candidates or #candidates == 0 then
        smppd.log("warn", "No route candidates available")
        return nil
    end

    smppd.log("debug", string.format(
        "Selecting route for destination=%s, %d candidates, strategy=%s",
        context.destination or "unknown",
        #candidates,
        config.strategy
    ))

    -- Filter to viable candidates
    local viable = filter_viable(candidates)

    -- Select based on strategy
    local selected = nil

    if config.strategy == "least_cost" then
        selected = select_least_cost(viable)
    elseif config.strategy == "quality_first" then
        selected = select_quality_first(viable)
    elseif config.strategy == "round_robin" then
        selected = select_round_robin(viable)
    else
        -- Default to balanced
        selected = select_balanced(viable)
    end

    if selected then
        smppd.log("info", string.format(
            "Selected route: %s (cluster=%s, cost=%s, quality=%s)",
            selected.route_name or "unknown",
            selected.cluster_name or "unknown",
            tostring(selected.cost or "N/A"),
            tostring(selected.quality_score or "N/A")
        ))
        return selected.cluster_name
    end

    return nil
end

return plugin
