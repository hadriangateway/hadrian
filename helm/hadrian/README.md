# Hadrian AI Gateway Helm Chart

A Helm chart for deploying [Hadrian AI Gateway](https://github.com/hadriangateway/hadrian) on Kubernetes.

## TL;DR

```bash
git clone https://github.com/hadriangateway/hadrian.git
cd gateway/helm/hadrian
helm dependency update
helm install my-gateway .
```

## Introduction

This chart deploys [Hadrian AI Gateway](https://github.com/hadriangateway/hadrian) on a Kubernetes cluster using the Helm package manager.

Hadrian is an open-source, OpenAI-compatible API gateway for routing requests to multiple LLM providers. It provides:

- Unified API for OpenAI, Anthropic, AWS Bedrock, Google Vertex AI, and Azure OpenAI
- Built-in web UI for chat and administration
- Multi-tenancy with organizations, teams, projects, and users
- Budget enforcement and usage tracking
- Knowledge bases / RAG with vector search
- Provider health checks, circuit breakers, and fallback routing
- Comprehensive observability with logging, metrics, and tracing

## Prerequisites

- Kubernetes 1.21+
- Helm 3.8+
- PV provisioner support (if using persistence)

### Optional Prerequisites

| Feature | Requirement |
|---------|-------------|
| Gateway API | [Gateway API CRDs](https://gateway-api.sigs.k8s.io/guides/#installing-gateway-api) v1.0+ |
| cert-manager | [cert-manager](https://cert-manager.io/docs/installation/) v1.0+ |
| Prometheus monitoring | [prometheus-operator](https://prometheus-operator.dev/) CRDs |
| Network policies | CNI plugin with NetworkPolicy support (Calico, Cilium, etc.) |

## Installation

```bash
# Clone the repository
git clone https://github.com/hadriangateway/hadrian.git
cd gateway/helm/hadrian

# Update dependencies
helm dependency update

# Install with default configuration
helm install my-gateway .

# Install with custom values
helm install my-gateway . -f values.yaml

# Install in a specific namespace
helm install my-gateway . -n hadrian --create-namespace
```

### Install with PostgreSQL and Redis

```bash
helm install my-gateway . \
  --set postgresql.enabled=true \
  --set redis.enabled=true \
  --set gateway.database.type=postgres \
  --set gateway.cache.type=redis
```

## Uninstallation

```bash
helm uninstall my-gateway

# If using PVCs, delete them manually if no longer needed
kubectl delete pvc -l app.kubernetes.io/instance=my-gateway
```

## Configuration

The following sections describe key configuration areas. See the [Parameters](#parameters) section for the complete list.

### Image Configuration

| Parameter | Description | Default |
|-----------|-------------|---------|
| `image.repository` | Container image repository | `ghcr.io/hadriangateway/hadrian` |
| `image.tag` | Image tag (defaults to Chart appVersion) | `""` |
| `image.pullPolicy` | Image pull policy | `IfNotPresent` |
| `imagePullSecrets` | Image pull secrets for private registries | `[]` |

### Database Configuration

Hadrian supports SQLite (default) and PostgreSQL.

#### SQLite (Development/Single Node)

```yaml
gateway:
  database:
    type: sqlite
    sqlite:
      path: "/app/data/hadrian.db"

persistence:
  enabled: true
  size: 1Gi
```

#### PostgreSQL Subchart (Recommended for Production)

```yaml
postgresql:
  enabled: true
  auth:
    username: gateway
    database: gateway
  primary:
    persistence:
      enabled: true
      size: 8Gi

gateway:
  database:
    type: postgres
```

#### External PostgreSQL (AWS RDS, Cloud SQL, Azure)

```yaml
gateway:
  database:
    type: postgres
    postgres:
      host: "mydb.abc123.us-east-1.rds.amazonaws.com"
      port: 5432
      database: gateway
      username: gateway
      existingSecret: rds-credentials  # Secret with 'password' key
      sslMode: verify-full
      ssl:
        enabled: true
        existingSecret: rds-ca-cert    # Secret with 'ca.crt' key
```

### Cache Configuration

Hadrian supports in-memory cache (default) and Redis.

#### Memory Cache (Development/Single Node)

```yaml
gateway:
  cache:
    type: memory
```

#### Redis Subchart (Recommended for Production)

```yaml
redis:
  enabled: true
  auth:
    enabled: true
  master:
    persistence:
      enabled: true
      size: 8Gi

gateway:
  cache:
    type: redis
```

#### External Redis (ElastiCache, MemoryStore, Azure Cache)

```yaml
gateway:
  cache:
    type: redis
    redis:
      host: "my-redis.abc123.cache.amazonaws.com"
      port: 6379
      existingSecret: elasticache-credentials
      tls:
        enabled: true
```

### Provider Configuration

Configure LLM providers with API keys stored in Kubernetes secrets.

```yaml
gateway:
  providers:
    defaultProvider: openrouter

    openrouter:
      enabled: true
      existingSecret: provider-secrets
      existingSecretKey: openrouter-api-key
      baseUrl: "https://openrouter.ai/api/v1/"

    openai:
      enabled: true
      existingSecret: provider-secrets
      existingSecretKey: openai-api-key

    anthropic:
      enabled: true
      existingSecret: provider-secrets
      existingSecretKey: anthropic-api-key
```

Create the secret:

```bash
kubectl create secret generic provider-secrets \
  --from-literal=openrouter-api-key=sk-or-xxx \
  --from-literal=openai-api-key=sk-xxx \
  --from-literal=anthropic-api-key=sk-ant-xxx
```

### Ingress Configuration

#### Standard Ingress

```yaml
ingress:
  enabled: true
  className: nginx
  annotations:
    nginx.ingress.kubernetes.io/proxy-body-size: "50m"
    nginx.ingress.kubernetes.io/proxy-read-timeout: "3600"
  hosts:
    - host: gateway.example.com
      paths:
        - path: /
          pathType: Prefix
  tls:
    - secretName: gateway-tls
      hosts:
        - gateway.example.com
```

#### Gateway API (HTTPRoute)

```yaml
gatewayAPI:
  enabled: true
  parentRefs:
    - name: my-gateway
      namespace: gateway-system
  hostnames:
    - gateway.example.com
```

#### TLS with cert-manager

```yaml
ingress:
  enabled: true
  className: nginx
  hosts:
    - host: gateway.example.com
      paths:
        - path: /
          pathType: Prefix

certManager:
  enabled: true
  issuer:
    name: letsencrypt-prod
    kind: ClusterIssuer
```

### High Availability

#### Multiple Replicas with HPA

```yaml
replicaCount: 3

autoscaling:
  enabled: true
  minReplicas: 3
  maxReplicas: 10
  targetCPUUtilizationPercentage: 80

podDisruptionBudget:
  enabled: true
  minAvailable: 2
```

#### Topology Spread Constraints

```yaml
topologySpreadConstraints:
  - maxSkew: 1
    topologyKey: topology.kubernetes.io/zone
    whenUnsatisfiable: DoNotSchedule
  - maxSkew: 1
    topologyKey: kubernetes.io/hostname
    whenUnsatisfiable: ScheduleAnyway
```

### Observability

#### Prometheus ServiceMonitor

```yaml
serviceMonitor:
  enabled: true
  labels:
    release: prometheus
  interval: 30s
```

#### Prometheus Alerting Rules

```yaml
prometheusRule:
  enabled: true
  labels:
    release: prometheus
  defaultRules:
    enabled: true
    critical:
      highErrorRate:
        enabled: true
        threshold: 0.05  # 5%
```

#### OpenTelemetry Tracing

```yaml
gateway:
  observability:
    tracing:
      enabled: true
      otlpEndpoint: "http://otel-collector:4317"
      serviceName: hadrian
```

### Security

#### Network Policy

```yaml
networkPolicy:
  enabled: true
  ingress:
    enabled: true
    allowSameNamespace: true
    ingressController:
      enabled: true
      namespace: ingress-nginx
    prometheus:
      enabled: true
      namespace: monitoring
  egress:
    enabled: true
    dns:
      enabled: true
    https:
      enabled: true
      excludePrivateRanges: true
```

#### Pod Security Context

The chart runs with secure defaults:

```yaml
podSecurityContext:
  fsGroup: 1000

securityContext:
  runAsNonRoot: true
  runAsUser: 1000
  runAsGroup: 1000
  allowPrivilegeEscalation: false
  readOnlyRootFilesystem: true
  capabilities:
    drop:
      - ALL
```

### Init Containers and Sidecars

#### Database Migrations

```yaml
initContainers:
  waitForDb:
    enabled: true
    timeoutSeconds: 60
  migrate:
    enabled: true
```

#### Vault Agent Sidecar

```yaml
sidecars:
  vaultAgent:
    enabled: true
    vaultAddr: "https://vault.example.com"
    role: hadrian-gateway
    templates:
      - name: api-keys
        destination: /vault/secrets/api-keys.env
        contents: |
          {{ with secret "secret/data/hadrian/providers" }}
          OPENAI_API_KEY={{ .Data.data.openai_api_key }}
          {{ end }}
```

#### Cloud SQL Auth Proxy

```yaml
sidecars:
  cloudSqlProxy:
    enabled: true
    instanceConnectionName: "project:region:instance"
    port: 5432
    credentials:
      useWorkloadIdentity: true

gateway:
  database:
    type: postgres
    postgres:
      host: "127.0.0.1"
      port: 5432
      sslMode: disable  # Proxy handles SSL
```

## Parameters

### Global Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `replicaCount` | Number of gateway replicas | `1` |
| `nameOverride` | Override the chart name | `""` |
| `fullnameOverride` | Override the full release name | `""` |

### Image Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `image.repository` | Container image repository | `ghcr.io/hadriangateway/hadrian` |
| `image.pullPolicy` | Image pull policy | `IfNotPresent` |
| `image.tag` | Image tag (defaults to appVersion) | `""` |
| `imagePullSecrets` | Image pull secrets | `[]` |

### Service Account Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `serviceAccount.create` | Create a service account | `true` |
| `serviceAccount.annotations` | Service account annotations | `{}` |
| `serviceAccount.name` | Service account name | `""` |
| `serviceAccount.automount` | Automount service account token | `true` |

### Pod Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `podAnnotations` | Pod annotations | `{}` |
| `podLabels` | Pod labels | `{}` |
| `podSecurityContext.fsGroup` | Pod filesystem group | `1000` |
| `securityContext.runAsNonRoot` | Run as non-root user | `true` |
| `securityContext.runAsUser` | Run as user ID | `1000` |
| `securityContext.runAsGroup` | Run as group ID | `1000` |
| `securityContext.readOnlyRootFilesystem` | Read-only root filesystem | `true` |

### Service Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `service.type` | Service type | `ClusterIP` |
| `service.port` | Service port | `80` |
| `service.targetPort` | Container port | `8080` |
| `service.nodePort` | Node port (if type=NodePort) | `""` |
| `service.annotations` | Service annotations | `{}` |

### Ingress Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `ingress.enabled` | Enable ingress | `false` |
| `ingress.className` | Ingress class name | `""` |
| `ingress.annotations` | Ingress annotations | `{}` |
| `ingress.hosts` | Ingress hosts configuration | See values.yaml |
| `ingress.tls` | TLS configuration | `[]` |

### Gateway API Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `gatewayAPI.enabled` | Enable Gateway API HTTPRoute | `false` |
| `gatewayAPI.parentRefs` | Parent Gateway references | `[]` |
| `gatewayAPI.hostnames` | Route hostnames | `["gateway.local"]` |
| `gatewayAPI.rules` | Routing rules | `[]` |

### cert-manager Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `certManager.enabled` | Enable cert-manager Certificate | `false` |
| `certManager.issuer.name` | Issuer name | `""` |
| `certManager.issuer.kind` | Issuer kind | `ClusterIssuer` |
| `certManager.secretName` | TLS secret name | `""` |
| `certManager.dnsNames` | DNS names for certificate | `[]` |

### Resource Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `resources.limits.cpu` | CPU limit | `2` |
| `resources.limits.memory` | Memory limit | `2Gi` |
| `resources.requests.cpu` | CPU request | `100m` |
| `resources.requests.memory` | Memory request | `256Mi` |

### Autoscaling Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `autoscaling.enabled` | Enable HPA | `false` |
| `autoscaling.minReplicas` | Minimum replicas | `1` |
| `autoscaling.maxReplicas` | Maximum replicas | `10` |
| `autoscaling.targetCPUUtilizationPercentage` | Target CPU utilization | `80` |
| `autoscaling.targetMemoryUtilizationPercentage` | Target memory utilization | `""` |

### Pod Disruption Budget Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `podDisruptionBudget.enabled` | Enable PDB | `false` |
| `podDisruptionBudget.minAvailable` | Minimum available pods | `""` |
| `podDisruptionBudget.maxUnavailable` | Maximum unavailable pods | `""` |

### Gateway Configuration Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `gateway.logLevel` | Log level | `info` |
| `gateway.server.host` | Server host | `0.0.0.0` |
| `gateway.server.port` | Server port | `8080` |
| `gateway.database.type` | Database type (sqlite/postgres) | `sqlite` |
| `gateway.cache.type` | Cache type (memory/redis) | `memory` |

### Provider Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `gateway.providers.defaultProvider` | Default provider | `openrouter` |
| `gateway.providers.openrouter.enabled` | Enable OpenRouter | `true` |
| `gateway.providers.openrouter.apiKey` | OpenRouter API key | `""` |
| `gateway.providers.openrouter.existingSecret` | Existing secret for API key | `""` |
| `gateway.providers.openai.enabled` | Enable OpenAI | `false` |
| `gateway.providers.anthropic.enabled` | Enable Anthropic | `false` |

### Persistence Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `persistence.enabled` | Enable SQLite persistence | `false` |
| `persistence.storageClass` | Storage class | `""` |
| `persistence.accessMode` | Access mode | `ReadWriteOnce` |
| `persistence.size` | Storage size | `1Gi` |
| `persistence.existingClaim` | Use existing PVC | `""` |

### File Storage Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `fileStorage.enabled` | Enable filesystem storage | `false` |
| `fileStorage.mountPath` | Mount path | `/var/hadrian/files` |
| `fileStorage.persistence.enabled` | Enable persistence | `true` |
| `fileStorage.persistence.size` | Storage size | `10Gi` |

### PostgreSQL Subchart Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `postgresql.enabled` | Enable PostgreSQL subchart | `false` |
| `postgresql.architecture` | Architecture (standalone/replication) | `standalone` |
| `postgresql.auth.username` | PostgreSQL username | `gateway` |
| `postgresql.auth.database` | PostgreSQL database | `gateway` |
| `postgresql.primary.persistence.size` | Storage size | `8Gi` |

### Redis Subchart Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `redis.enabled` | Enable Redis subchart | `false` |
| `redis.architecture` | Architecture (standalone/replication) | `standalone` |
| `redis.auth.enabled` | Enable authentication | `true` |
| `redis.master.persistence.size` | Storage size | `8Gi` |

### Monitoring Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `serviceMonitor.enabled` | Enable ServiceMonitor | `false` |
| `serviceMonitor.labels` | ServiceMonitor labels | `{}` |
| `serviceMonitor.interval` | Scrape interval | `30s` |
| `podMonitor.enabled` | Enable PodMonitor | `false` |
| `prometheusRule.enabled` | Enable PrometheusRule | `false` |
| `prometheusRule.defaultRules.enabled` | Enable default alerting rules | `true` |

### Network Policy Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `networkPolicy.enabled` | Enable NetworkPolicy | `false` |
| `networkPolicy.ingress.enabled` | Enable ingress rules | `true` |
| `networkPolicy.ingress.allowSameNamespace` | Allow same namespace | `true` |
| `networkPolicy.egress.enabled` | Enable egress rules | `true` |
| `networkPolicy.egress.dns.enabled` | Allow DNS egress | `true` |
| `networkPolicy.egress.https.enabled` | Allow HTTPS egress | `true` |

### Init Container Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `initContainers.migrate.enabled` | Enable migration init container | `false` |
| `initContainers.waitForDb.enabled` | Enable wait-for-db init container | `false` |
| `initContainers.waitForDb.timeoutSeconds` | Database wait timeout | `60` |

### Sidecar Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `sidecars.vaultAgent.enabled` | Enable Vault Agent sidecar | `false` |
| `sidecars.cloudSqlProxy.enabled` | Enable Cloud SQL Proxy sidecar | `false` |
| `sidecars.oauth2Proxy.enabled` | Enable OAuth2 Proxy sidecar | `false` |

## Examples

### Development Setup (SQLite + Memory Cache)

```yaml
# values-dev.yaml
replicaCount: 1

gateway:
  database:
    type: sqlite
  cache:
    type: memory
  providers:
    openrouter:
      enabled: true
      apiKey: "sk-or-dev-xxx"  # Use existingSecret in production

persistence:
  enabled: true
  size: 1Gi

service:
  type: NodePort
```

### Production Setup (PostgreSQL + Redis + HA)

```yaml
# values-prod.yaml
replicaCount: 3

postgresql:
  enabled: true
  auth:
    existingSecret: postgres-credentials
  primary:
    persistence:
      enabled: true
      size: 50Gi
    resources:
      requests:
        cpu: 500m
        memory: 1Gi
      limits:
        cpu: 2
        memory: 4Gi

redis:
  enabled: true
  auth:
    existingSecret: redis-credentials
  master:
    persistence:
      enabled: true
      size: 10Gi

gateway:
  database:
    type: postgres
  cache:
    type: redis
  providers:
    openrouter:
      enabled: true
      existingSecret: provider-secrets
    openai:
      enabled: true
      existingSecret: provider-secrets

autoscaling:
  enabled: true
  minReplicas: 3
  maxReplicas: 10
  targetCPUUtilizationPercentage: 70

podDisruptionBudget:
  enabled: true
  minAvailable: 2

topologySpreadConstraints:
  - maxSkew: 1
    topologyKey: topology.kubernetes.io/zone
    whenUnsatisfiable: DoNotSchedule

ingress:
  enabled: true
  className: nginx
  annotations:
    nginx.ingress.kubernetes.io/proxy-body-size: "100m"
    nginx.ingress.kubernetes.io/proxy-read-timeout: "3600"
  hosts:
    - host: gateway.example.com
      paths:
        - path: /
          pathType: Prefix

certManager:
  enabled: true
  issuer:
    name: letsencrypt-prod
    kind: ClusterIssuer

serviceMonitor:
  enabled: true
  labels:
    release: prometheus

prometheusRule:
  enabled: true
  labels:
    release: prometheus

networkPolicy:
  enabled: true
  ingress:
    ingressController:
      enabled: true
      namespace: ingress-nginx
    prometheus:
      enabled: true
      namespace: monitoring

initContainers:
  waitForDb:
    enabled: true
  migrate:
    enabled: true

resources:
  requests:
    cpu: 500m
    memory: 512Mi
  limits:
    cpu: 2
    memory: 2Gi
```

### AWS EKS with RDS and ElastiCache

```yaml
# values-eks.yaml
serviceAccount:
  annotations:
    eks.amazonaws.com/role-arn: arn:aws:iam::123456789:role/hadrian-gateway

gateway:
  database:
    type: postgres
    postgres:
      host: "hadrian.abc123.us-east-1.rds.amazonaws.com"
      existingSecret: rds-credentials
      sslMode: verify-full
      ssl:
        enabled: true
        existingSecret: rds-ca-cert
  cache:
    type: redis
    redis:
      host: "hadrian.abc123.cache.amazonaws.com"
      port: 6379
      existingSecret: elasticache-credentials
      tls:
        enabled: true

ingress:
  enabled: true
  className: alb
  annotations:
    alb.ingress.kubernetes.io/scheme: internet-facing
    alb.ingress.kubernetes.io/target-type: ip
    alb.ingress.kubernetes.io/certificate-arn: arn:aws:acm:us-east-1:123456789:certificate/xxx
```

### GKE with Cloud SQL and Workload Identity

```yaml
# values-gke.yaml
serviceAccount:
  annotations:
    iam.gke.io/gcp-service-account: hadrian@project.iam.gserviceaccount.com

sidecars:
  cloudSqlProxy:
    enabled: true
    instanceConnectionName: "project:us-central1:hadrian-db"
    credentials:
      useWorkloadIdentity: true

gateway:
  database:
    type: postgres
    postgres:
      host: "127.0.0.1"
      port: 5432
      existingSecret: cloudsql-credentials
      sslMode: disable

ingress:
  enabled: true
  className: gce
  annotations:
    kubernetes.io/ingress.global-static-ip-name: hadrian-ip
    networking.gke.io/managed-certificates: hadrian-cert
```

## Upgrading

### To 0.2.0

No breaking changes.

### To 0.1.0

Initial release.

## Troubleshooting

### Pod Not Starting

Check pod events and logs:

```bash
kubectl describe pod -l app.kubernetes.io/name=hadrian
kubectl logs -l app.kubernetes.io/name=hadrian --all-containers
```

### Database Connection Issues

1. Verify database credentials:
   ```bash
   kubectl get secret <secret-name> -o yaml
   ```

2. Check if database is accessible:
   ```bash
   kubectl exec -it deploy/<release-name>-hadrian -- nc -zv <db-host> <db-port>
   ```

3. For PostgreSQL subchart, check its status:
   ```bash
   kubectl get pods -l app.kubernetes.io/name=postgresql
   kubectl logs -l app.kubernetes.io/name=postgresql
   ```

### Ingress Not Working

1. Check ingress status:
   ```bash
   kubectl get ingress
   kubectl describe ingress <release-name>-hadrian
   ```

2. Verify ingress controller logs:
   ```bash
   kubectl logs -n ingress-nginx -l app.kubernetes.io/name=ingress-nginx
   ```

### Prometheus Not Scraping

1. Verify ServiceMonitor exists:
   ```bash
   kubectl get servicemonitor
   ```

2. Check Prometheus targets:
   ```bash
   kubectl port-forward -n monitoring svc/prometheus-operated 9090:9090
   # Visit http://localhost:9090/targets
   ```

3. Ensure ServiceMonitor labels match Prometheus selector.

## License

This project is dual-licensed under [Apache 2.0](https://github.com/hadriangateway/hadrian/blob/main/LICENSE-APACHE) and [MIT](https://github.com/hadriangateway/hadrian/blob/main/LICENSE-MIT). Choose whichever you prefer.
