import numpy as np

iterations = 100_000

# Stage 1: Auth service latency (Normal distribution: mean 40ms, stddev 5ms)
auth_latency = np.random.normal(loc=40, scale=5, size=iterations)

# Stage 2: Database query latency (Lognormal distribution for tail risk)
db_latency = np.random.lognormal(mean=3.8, sigma=0.4, size=iterations)

# Stage 3: Third-party API call (Uniform distribution between 20ms and 100ms)
api_latency = np.random.uniform(low=20, high=100, size=iterations)

# Aggregate total end-to-end pipeline latency across all runs
total_latency = auth_latency + db_latency + api_latency

# Calculate percentiles and SLA compliance
p50 = np.percentile(total_latency, 50)
p95 = np.percentile(total_latency, 95)
sla_met = np.mean(total_latency <= 200) * 100

print(f"p50 Latency: {p50:.2f}ms")
print(f"p95 Latency: {p95:.2f}ms")
print(f"Probability of meeting <200ms SLA: {sla_met:.2f}%")
