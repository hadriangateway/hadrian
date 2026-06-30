//! Document processing pipeline for vector stores.
//!
//! This module handles chunking documents into semantically meaningful segments
//! and generating embeddings for vector search.
//!
//! # Chunking Strategies
//!
//! - **Auto**: Intelligent chunking based on content structure (paragraphs, sections)
//! - **Static**: Fixed-size chunks with configurable overlap
//!
//! # Supported File Types
//!
//! ## Plain Text (direct UTF-8)
//! - Text: `.txt`, `.md`, `.json`, `.csv`, `.xml`, `.html`
//! - Code: `.rs`, `.py`, `.js`, `.ts`, `.go`, `.java`, `.c`, `.cpp`, `.h`, etc.
//! - Config: `.yaml`, `.toml`, `.ini`, `.env`
//!
//! ## Rich Documents (via xberg)
//! - PDF: `.pdf`
//! - Microsoft Office: `.docx`, `.doc`, `.xlsx`, `.xls`, `.pptx`, `.ppt`
//! - OpenDocument: `.odt`, `.ods`, `.odp`
//! - Other: `.rtf`, `.epub`
//!
//! ## Images with OCR (via xberg + Tesseract)
//! - `.png`, `.jpg`, `.jpeg`, `.tiff`, `.bmp`, `.webp`, `.gif`
//!
//! OCR requires Tesseract to be installed on the system. Configure via `document_extraction`:
//! - `enable_ocr`: Enable OCR for images and scanned documents (default: false)
//! - `force_ocr`: Force OCR even for text-based PDFs (default: false)
//! - `ocr_language`: Tesseract language code, e.g., "eng" (default: "eng")

use std::{sync::Arc, time::Instant};

use thiserror::Error;
use tiktoken_rs::{CoreBPE, cl100k_base};
use tokio::sync::Semaphore;
use tracing::{Instrument, debug, error, info, info_span, instrument, warn};
use uuid::Uuid;

use crate::{
    cache::{EmbeddingService, vector_store::VectorBackend},
    config::{
        DocumentExtractionConfig, FileProcessingConfig, FileProcessingMode,
        FileProcessingQueueBackend, FileProcessingQueueConfig,
    },
    db::DbPool,
    models::{ChunkingStrategy, FileError, FileErrorCode, VectorStoreFileStatus},
    observability::{metrics::record_document_processing, otel_span_error, otel_span_ok},
    providers::{
        circuit_breaker::CircuitBreaker,
        retry::{is_retryable_database_error, with_circuit_breaker_and_retry_generic},
    },
    services::VectorStoresService,
};

/// Errors that can occur during document processing.
#[derive(Debug, Error)]
pub enum DocumentProcessorError {
    #[error("File not found: {0}")]
    FileNotFound(Uuid),

    #[error("Unsupported file type: {0}")]
    UnsupportedFileType(String),

    #[error("File too large: {size} bytes (max: {max} bytes)")]
    FileTooLarge { size: i64, max: i64 },

    #[error("Invalid UTF-8 content")]
    InvalidUtf8,

    #[error("Document extraction failed: {0}")]
    DocumentExtraction(String),

    #[error("Embedding error: {0}")]
    Embedding(String),

    #[error("Vector store error: {0}")]
    VectorStore(String),

    #[error("Database error: {0}")]
    Database(String),

    #[error("Tokenization error: {0}")]
    Tokenization(String),

    #[error("Vector store circuit breaker is open: {0}")]
    CircuitBreakerOpen(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Configuration error: {0}")]
    Configuration(String),
}

impl From<crate::cache::EmbeddingError> for DocumentProcessorError {
    fn from(err: crate::cache::EmbeddingError) -> Self {
        DocumentProcessorError::Embedding(err.to_string())
    }
}

impl From<crate::cache::vector_store::VectorStoreError> for DocumentProcessorError {
    fn from(err: crate::cache::vector_store::VectorStoreError) -> Self {
        DocumentProcessorError::VectorStore(err.to_string())
    }
}

impl From<crate::db::DbError> for DocumentProcessorError {
    fn from(err: crate::db::DbError) -> Self {
        DocumentProcessorError::Database(err.to_string())
    }
}

/// Processing mode for document processing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ProcessingMode {
    /// Process files inline in the gateway process (default)
    #[default]
    Inline,
    /// Publish processing jobs to a queue for external workers
    Queue,
}

/// Queue backend configuration for queue-based processing.
#[derive(Debug, Clone)]
pub enum QueueBackend {
    /// Redis Streams
    Redis {
        url: String,
        queue_name: String,
        consumer_group: String,
    },
}

/// Configuration for the document processor.
#[derive(Debug, Clone)]
pub struct DocumentProcessorConfig {
    /// Maximum file size in bytes (default: 10MB)
    pub max_file_size: i64,
    /// Maximum concurrent file processing tasks (for inline mode)
    pub max_concurrent_tasks: usize,
    /// Default max chunk size in tokens when using auto strategy
    pub default_max_chunk_tokens: i32,
    /// Default chunk overlap in tokens when using auto strategy
    pub default_overlap_tokens: i32,
    /// Processing mode: inline or queue-based
    pub processing_mode: ProcessingMode,
    /// Queue backend configuration (required when processing_mode = Queue)
    pub queue_backend: Option<QueueBackend>,
    /// Callback URL for queue workers to report completion
    pub callback_url: Option<String>,
    /// Document extraction configuration (OCR, PDF options)
    pub document_extraction: DocumentExtractionConfig,
    /// Retry configuration for vector store operations
    pub retry: crate::config::RetryConfig,
    /// Circuit breaker configuration for vector store operations
    pub circuit_breaker: crate::config::CircuitBreakerConfig,
}

impl Default for DocumentProcessorConfig {
    fn default() -> Self {
        Self {
            max_file_size: 10 * 1024 * 1024, // 10MB
            max_concurrent_tasks: 4,
            default_max_chunk_tokens: 800,
            default_overlap_tokens: 200,
            processing_mode: ProcessingMode::Inline,
            queue_backend: None,
            callback_url: None,
            document_extraction: DocumentExtractionConfig::default(),
            retry: crate::config::RetryConfig::default(),
            circuit_breaker: crate::config::CircuitBreakerConfig::default(),
        }
    }
}

impl From<&FileProcessingConfig> for DocumentProcessorConfig {
    fn from(config: &FileProcessingConfig) -> Self {
        Self {
            max_file_size: config.max_file_size_bytes(),
            max_concurrent_tasks: config.max_concurrent_tasks,
            default_max_chunk_tokens: config.default_max_chunk_tokens,
            default_overlap_tokens: config.default_overlap_tokens,
            processing_mode: match config.mode {
                FileProcessingMode::Inline => ProcessingMode::Inline,
                FileProcessingMode::Queue => ProcessingMode::Queue,
            },
            queue_backend: config.queue.as_ref().map(convert_queue_config),
            callback_url: config.callback_url.clone(),
            document_extraction: config.document_extraction.clone(),
            retry: config.retry.clone(),
            circuit_breaker: config.circuit_breaker.clone(),
        }
    }
}

/// Convert FileProcessingQueueConfig to QueueBackend.
fn convert_queue_config(queue: &FileProcessingQueueConfig) -> QueueBackend {
    match queue.backend {
        FileProcessingQueueBackend::Redis => QueueBackend::Redis {
            url: queue.url.clone(),
            queue_name: queue.queue_name.clone(),
            consumer_group: queue.consumer_group.clone(),
        },
    }
}

/// Job message for queue-based document processing.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProcessingJob {
    /// Unique job ID
    pub job_id: Uuid,
    /// File to process
    pub file_id: Uuid,
    /// VectorStore the file belongs to
    pub vector_store_id: Uuid,
    /// Storage backend where the file is stored
    pub storage_backend: String,
    /// Storage path (for filesystem/S3 backends)
    pub storage_path: Option<String>,
    /// Chunking strategy to use
    pub chunking_strategy: Option<ChunkingStrategy>,
    /// Callback URL to report completion
    pub callback_url: Option<String>,
}

/// A text chunk with position and token information.
#[derive(Debug, Clone)]
pub struct TextChunk {
    /// The chunk content
    pub content: String,
    /// Character offset in original document
    pub char_start: i32,
    /// Character end offset in original document
    pub char_end: i32,
    /// Token count for this chunk
    pub token_count: i32,
    /// Chunk index in sequence
    pub index: i32,
}

/// Document processor for chunking and embedding files.
pub struct DocumentProcessor {
    db: Arc<DbPool>,
    vector_stores_service: Arc<VectorStoresService>,
    embedding_service: Option<Arc<EmbeddingService>>,
    vector_store: Option<Arc<dyn VectorBackend>>,
    config: DocumentProcessorConfig,
    tokenizer: CoreBPE,
    semaphore: Semaphore,
    circuit_breaker: Option<Arc<CircuitBreaker>>,
}

impl DocumentProcessor {
    /// Create a new document processor.
    ///
    /// # Arguments
    /// * `db` - Database pool for accessing file data
    /// * `vector_stores_service` - Service for managing collections and files
    /// * `embedding_service` - Optional embedding service for generating vectors
    /// * `vector_store` - Optional vector store for storing embeddings
    /// * `config` - Processor configuration
    pub fn new(
        db: Arc<DbPool>,
        vector_stores_service: Arc<VectorStoresService>,
        embedding_service: Option<Arc<EmbeddingService>>,
        vector_store: Option<Arc<dyn VectorBackend>>,
        config: DocumentProcessorConfig,
    ) -> Result<Self, DocumentProcessorError> {
        let tokenizer =
            cl100k_base().map_err(|e| DocumentProcessorError::Tokenization(e.to_string()))?;

        let circuit_breaker = if config.circuit_breaker.enabled {
            Some(Arc::new(CircuitBreaker::new(
                "document_processor_vector_store",
                &config.circuit_breaker,
            )))
        } else {
            None
        };

        Ok(Self {
            db,
            vector_stores_service,
            embedding_service,
            vector_store,
            semaphore: Semaphore::new(config.max_concurrent_tasks),
            config,
            tokenizer,
            circuit_breaker,
        })
    }

    /// Process a vector store file: extract text, chunk it, generate embeddings, and store.
    ///
    /// This is the main entry point for file processing. It:
    /// 1. Retrieves the vector store file link and actual file data
    /// 2. Extracts text content
    /// 3. Chunks the text according to the configured strategy
    /// 4. Generates embeddings for each chunk
    /// 5. Stores chunks with embeddings in the vector store
    /// 6. Updates file status
    ///
    /// # Arguments
    /// * `vector_store_file_id` - The ID of the vector_store_files entry (not the files entry)
    ///
    /// Returns the number of chunks created.
    #[instrument(skip(self), fields(vector_store_file_id = %vector_store_file_id, file_id, vector_store_id))]
    pub async fn process_file(
        &self,
        vector_store_file_id: Uuid,
    ) -> Result<usize, DocumentProcessorError> {
        // Acquire semaphore to limit concurrency
        let _permit =
            self.semaphore.acquire().await.map_err(|_| {
                DocumentProcessorError::Internal("Processing semaphore closed".into())
            })?;

        // Start timing for metrics and stage tracking
        let start_time = Instant::now();
        let mut stage_start = Instant::now();

        info!(
            stage = "processing_started",
            "Starting document processing pipeline"
        );

        // Get vector store file link (has file_id reference and chunking strategy)
        let vector_store_file = match self
            .vector_stores_service
            .get_vector_store_file(vector_store_file_id)
            .await
        {
            Ok(Some(cf)) => cf,
            Ok(None) => {
                otel_span_error!("VectorStore file not found");
                return Err(DocumentProcessorError::FileNotFound(vector_store_file_id));
            }
            Err(e) => {
                otel_span_error!("Database error: {}", e);
                return Err(e.into());
            }
        };

        // Get actual file metadata from Files API
        let file = match self.db.files().get_file(vector_store_file.file_id).await {
            Ok(Some(f)) => f,
            Ok(None) => {
                otel_span_error!("File not found");
                return Err(DocumentProcessorError::FileNotFound(
                    vector_store_file.file_id,
                ));
            }
            Err(e) => {
                otel_span_error!("Database error: {}", e);
                return Err(e.into());
            }
        };

        // Get file extension early for metrics reporting
        let extension = get_file_extension(&file.filename);

        // Structured context for all subsequent log events
        let file_id = vector_store_file.file_id;
        let vector_store_id = vector_store_file.vector_store_id;

        // Record file_id and vector_store_id in the parent span for distributed tracing
        tracing::Span::current().record("file_id", file_id.to_string());
        tracing::Span::current().record("vector_store_id", vector_store_id.to_string());
        let file_size_bytes = file.size_bytes as u64;

        // Validation stage span
        let validation_span = info_span!(
            "validate_file",
            file_type = %extension,
            file_size_bytes = file_size_bytes,
            max_size_bytes = self.config.max_file_size as u64
        );
        let _validation_guard = validation_span.enter();

        // Validate file size
        if file.size_bytes > self.config.max_file_size {
            info!(
                stage = "validation_failed",
                file_id = %file_id,
                vector_store_id = %vector_store_id,
                file_type = %extension,
                file_size_bytes = file_size_bytes,
                max_size_bytes = self.config.max_file_size as u64,
                reason = "file_too_large",
                "File validation failed"
            );
            self.update_file_error(
                vector_store_file_id,
                FileErrorCode::InvalidFile,
                &format!(
                    "File size {} exceeds maximum {}",
                    file.size_bytes, self.config.max_file_size
                ),
            )
            .await?;
            record_document_processing(
                "error",
                start_time.elapsed().as_secs_f64(),
                0,
                file_size_bytes,
                &extension,
            );
            otel_span_error!("File too large");
            return Err(DocumentProcessorError::FileTooLarge {
                size: file.size_bytes,
                max: self.config.max_file_size,
            });
        }

        // Validate file type
        if !is_supported_file_type(&extension) {
            info!(
                stage = "validation_failed",
                file_id = %file_id,
                vector_store_id = %vector_store_id,
                file_type = %extension,
                file_size_bytes = file_size_bytes,
                reason = "unsupported_file_type",
                "File validation failed"
            );
            self.update_file_error(
                vector_store_file_id,
                FileErrorCode::UnsupportedFile,
                &format!("File type '{}' is not supported", extension),
            )
            .await?;
            record_document_processing(
                "error",
                start_time.elapsed().as_secs_f64(),
                0,
                file_size_bytes,
                &extension,
            );
            otel_span_error!("Unsupported file type");
            return Err(DocumentProcessorError::UnsupportedFileType(extension));
        }

        info!(
            stage = "file_validated",
            file_id = %file_id,
            vector_store_id = %vector_store_id,
            file_type = %extension,
            file_size_bytes = file_size_bytes,
            duration_ms = stage_start.elapsed().as_millis() as u64,
            "File validation completed"
        );

        // Drop validation span before moving to next stage
        drop(_validation_guard);
        stage_start = Instant::now();

        // Text extraction stage span
        let extraction_span = info_span!("extract_text", file_type = %extension);
        let text = {
            let _extraction_guard = extraction_span.enter();

            // Get file content from Files API
            let file_data = match self
                .db
                .files()
                .get_file_data(vector_store_file.file_id)
                .await
            {
                Ok(Some(data)) => data,
                Ok(None) => {
                    otel_span_error!("File data not found");
                    return Err(DocumentProcessorError::FileNotFound(
                        vector_store_file.file_id,
                    ));
                }
                Err(e) => {
                    otel_span_error!("Database error: {}", e);
                    return Err(e.into());
                }
            };

            // Extract text - takes ownership of file_data to avoid copying
            match extract_text(file_data, &extension, &self.config.document_extraction).await {
                Ok(text) => text,
                Err(e) => {
                    info!(
                        stage = "extraction_failed",
                        file_id = %file_id,
                        vector_store_id = %vector_store_id,
                        file_type = %extension,
                        file_size_bytes = file_size_bytes,
                        error = %e,
                        "Text extraction failed"
                    );
                    self.update_file_error(
                        vector_store_file_id,
                        FileErrorCode::InvalidFile,
                        &e.to_string(),
                    )
                    .await?;
                    record_document_processing(
                        "error",
                        start_time.elapsed().as_secs_f64(),
                        0,
                        file_size_bytes,
                        &extension,
                    );
                    otel_span_error!("Text extraction failed");
                    return Err(e);
                }
            }
        };

        let text_len = text.len();
        info!(
            stage = "text_extracted",
            file_id = %file_id,
            vector_store_id = %vector_store_id,
            file_type = %extension,
            file_size_bytes = file_size_bytes,
            text_length = text_len,
            duration_ms = stage_start.elapsed().as_millis() as u64,
            "Text extraction completed"
        );
        stage_start = Instant::now();

        // Determine chunking strategy (from vector_store_file, not the file itself)
        let strategy = vector_store_file
            .chunking_strategy
            .clone()
            .unwrap_or_default();
        let (max_tokens, overlap_tokens) = match strategy {
            ChunkingStrategy::Auto => (
                self.config.default_max_chunk_tokens,
                self.config.default_overlap_tokens,
            ),
            ChunkingStrategy::Static { config } => {
                (config.max_chunk_size_tokens, config.chunk_overlap_tokens)
            }
        };

        // Chunking stage span
        let chunking_span = info_span!(
            "chunk_text",
            text_length = text_len,
            max_tokens = max_tokens,
            overlap_tokens = overlap_tokens
        );
        let chunks =
            chunking_span.in_scope(|| self.chunk_text(&text, max_tokens, overlap_tokens))?;
        let chunk_count = chunks.len();

        info!(
            stage = "chunking_completed",
            file_id = %file_id,
            vector_store_id = %vector_store_id,
            file_type = %extension,
            file_size_bytes = file_size_bytes,
            chunk_count = chunk_count,
            max_tokens = max_tokens,
            overlap_tokens = overlap_tokens,
            duration_ms = stage_start.elapsed().as_millis() as u64,
            "Text chunking completed"
        );
        stage_start = Instant::now();

        if chunks.is_empty() {
            // Empty file, mark as completed
            info!(
                stage = "processing_completed",
                file_id = %file_id,
                vector_store_id = %vector_store_id,
                file_type = %extension,
                file_size_bytes = file_size_bytes,
                chunk_count = 0,
                stored_count = 0,
                total_duration_ms = start_time.elapsed().as_millis() as u64,
                "Document processing completed (empty file)"
            );
            self.vector_stores_service
                .update_vector_store_file_status(
                    vector_store_file_id,
                    VectorStoreFileStatus::Completed,
                    None,
                )
                .await?;
            record_document_processing(
                "success",
                start_time.elapsed().as_secs_f64(),
                0,
                file_size_bytes,
                &extension,
            );
            otel_span_ok!();
            return Ok(0);
        }

        // Generate a shared processing version for all chunks in this run.
        // This enables atomic shadow-copy updates: new chunks are stored first,
        // then old chunks (with different versions) are deleted only after success.
        let processing_version = Uuid::new_v4();

        // Generate embeddings and store chunks in vector store
        // Process chunks incrementally - each chunk is dropped after storage,
        // allowing memory to be reclaimed progressively rather than holding
        // all chunks in memory until the entire file is processed.

        // Embedding stage parent span
        let embedding_span = info_span!("embed_and_store_chunks", chunk_count = chunk_count);

        let (stored_count, failed_count, usage_bytes) = async {
            info!(
                stage = "embedding_started",
                file_id = %file_id,
                vector_store_id = %vector_store_id,
                file_type = %extension,
                chunk_count = chunk_count,
                "Starting embedding generation and storage"
            );

            let mut stored_count = 0usize;
            let mut failed_count = 0usize;
            let mut usage_bytes = 0i64;

            // Get file attributes for inclusion in chunk metadata
            let file_attributes = vector_store_file.attributes.as_ref();

            for chunk in chunks {
                // Track usage before moving chunk content
                let chunk_size = chunk.content.len() as i64;
                let chunk_index = chunk.index;

                if let (Some(embedding_service), Some(vector_store)) =
                    (&self.embedding_service, &self.vector_store)
                {
                    match self
                        .generate_and_store_chunk(
                            embedding_service,
                            vector_store.as_ref(),
                            vector_store_id,
                            file_id,
                            chunk,
                            file_attributes,
                            processing_version,
                        )
                        .await
                    {
                        Ok(()) => {
                            stored_count += 1;
                            debug!(
                                stage = "chunk_stored",
                                file_id = %file_id,
                                vector_store_id = %vector_store_id,
                                chunk_index = chunk_index,
                                chunk_size = chunk_size,
                                "Chunk embedded and stored"
                            );
                        }
                        Err(e) => {
                            failed_count += 1;
                            warn!(
                                stage = "chunk_storage_failed",
                                file_id = %file_id,
                                vector_store_id = %vector_store_id,
                                chunk_index = chunk_index,
                                error = %e,
                                "Failed to store chunk"
                            );
                        }
                    }
                } else {
                    debug!(
                        file_id = %file_id,
                        "No embedding service configured, skipping chunk storage"
                    );
                }

                usage_bytes += chunk_size;
            }

            info!(
                stage = "embedding_completed",
                file_id = %file_id,
                vector_store_id = %vector_store_id,
                file_type = %extension,
                chunk_count = chunk_count,
                stored_count = stored_count,
                failed_count = failed_count,
                duration_ms = stage_start.elapsed().as_millis() as u64,
                "Embedding generation and storage completed"
            );

            (stored_count, failed_count, usage_bytes)
        }
        .instrument(embedding_span)
        .await;

        // Shadow-copy cleanup: delete old version chunks only after all new chunks succeeded.
        // This ensures atomicity - if chunk storage fails, old chunks remain intact.
        if failed_count == 0
            && stored_count > 0
            && let Some(vector_store) = &self.vector_store
        {
            let cleanup_result = with_circuit_breaker_and_retry_generic(
                self.circuit_breaker.as_deref(),
                &self.config.retry,
                "vector_store",
                "delete_old_chunks",
                |e: &crate::cache::vector_store::VectorStoreError| match e {
                    crate::cache::vector_store::VectorStoreError::Database(msg) => {
                        is_retryable_database_error(msg)
                    }
                    crate::cache::vector_store::VectorStoreError::Http(_) => true,
                    _ => false,
                },
                |_| false,
                || async {
                    vector_store
                        .delete_chunks_by_file_and_vector_store_except_version(
                            file_id,
                            vector_store_id,
                            processing_version,
                        )
                        .await
                },
            )
            .await;

            match cleanup_result {
                Ok(deleted) => {
                    if deleted > 0 {
                        debug!(
                            file_id = %file_id,
                            vector_store_id = %vector_store_id,
                            deleted_count = deleted,
                            processing_version = %processing_version,
                            "Deleted old chunk versions after successful processing"
                        );
                    }
                }
                Err(e) => {
                    // Cleanup failure is not fatal - new chunks are stored successfully.
                    // Old chunks will be orphaned but won't affect search results
                    // (they have the same content, just different version).
                    // A future re-processing or cleanup job can remove them.
                    match e {
                        crate::providers::retry::GenericRequestError::CircuitBreakerOpen(
                            cb_err,
                        ) => {
                            warn!(
                                error = %cb_err,
                                file_id = %file_id,
                                vector_store_id = %vector_store_id,
                                "Circuit breaker open, old chunk cleanup skipped"
                            );
                        }
                        crate::providers::retry::GenericRequestError::Operation(op_err) => {
                            warn!(
                                error = %op_err,
                                file_id = %file_id,
                                vector_store_id = %vector_store_id,
                                "Failed to delete old chunk versions, orphaned chunks may exist"
                            );
                        }
                    }
                }
            }
        }

        // Update file status and usage
        self.vector_stores_service
            .update_vector_store_file_usage(vector_store_file_id, usage_bytes)
            .await?;

        // Determine final status based on chunk storage results
        let (final_status, file_error, metrics_status) = if failed_count > 0 {
            if stored_count == 0 {
                // Complete failure - no chunks stored
                (
                    VectorStoreFileStatus::Failed,
                    Some(FileError {
                        code: FileErrorCode::ServerError,
                        message: format!("All {} chunks failed to store", failed_count),
                    }),
                    "error",
                )
            } else {
                // Partial failure - some chunks stored, some failed
                (
                    VectorStoreFileStatus::Failed,
                    Some(FileError {
                        code: FileErrorCode::ServerError,
                        message: format!(
                            "Partial failure: {} of {} chunks stored, {} failed",
                            stored_count,
                            stored_count + failed_count,
                            failed_count
                        ),
                    }),
                    "partial_failure",
                )
            }
        } else {
            (VectorStoreFileStatus::Completed, None, "success")
        };

        self.vector_stores_service
            .update_vector_store_file_status(vector_store_file_id, final_status, file_error)
            .await?;

        // Record processing metrics
        record_document_processing(
            metrics_status,
            start_time.elapsed().as_secs_f64(),
            stored_count as u32,
            file_size_bytes,
            &extension,
        );

        if failed_count > 0 {
            warn!(
                stage = "processing_completed",
                status = "failed",
                file_id = %file_id,
                vector_store_id = %vector_store_id,
                file_type = %extension,
                file_size_bytes = file_size_bytes,
                chunk_count = chunk_count,
                stored_count = stored_count,
                failed_count = failed_count,
                usage_bytes = usage_bytes,
                total_duration_ms = start_time.elapsed().as_millis() as u64,
                "Document processing completed with failures"
            );
            otel_span_error!("Chunk storage failures");
        } else {
            info!(
                stage = "processing_completed",
                status = "completed",
                file_id = %file_id,
                vector_store_id = %vector_store_id,
                file_type = %extension,
                file_size_bytes = file_size_bytes,
                chunk_count = chunk_count,
                stored_count = stored_count,
                usage_bytes = usage_bytes,
                total_duration_ms = start_time.elapsed().as_millis() as u64,
                "Document processing pipeline completed"
            );
            otel_span_ok!();
        }

        Ok(stored_count)
    }

    /// Chunk text into segments based on token count.
    ///
    /// Uses a semantic-aware approach:
    /// 1. Split by paragraphs (double newlines)
    /// 2. Merge small paragraphs, split large ones
    /// 3. Add overlap between chunks for context continuity
    pub fn chunk_text(
        &self,
        text: &str,
        max_tokens: i32,
        overlap_tokens: i32,
    ) -> Result<Vec<TextChunk>, DocumentProcessorError> {
        if text.is_empty() {
            return Ok(vec![]);
        }

        let max_tokens = max_tokens as usize;
        let overlap_tokens = overlap_tokens as usize;

        // Split by paragraphs first (semantic boundaries)
        let paragraphs: Vec<&str> = text.split("\n\n").collect();

        let mut chunks = Vec::new();
        let mut current_chunk = String::new();
        let mut current_tokens = 0usize;
        let mut current_start = 0i32;
        let mut char_offset = 0i32;

        for (para_idx, para) in paragraphs.iter().enumerate() {
            let para_tokens = self.count_tokens(para);
            let separator = if para_idx > 0 { "\n\n" } else { "" };
            let separator_len = separator.len() as i32;

            if para_tokens > max_tokens {
                // Paragraph is too large, need to split it
                if !current_chunk.is_empty() {
                    // Save current chunk first
                    let chunk_end = char_offset;
                    chunks.push(TextChunk {
                        content: current_chunk.clone(),
                        char_start: current_start,
                        char_end: chunk_end,
                        token_count: current_tokens as i32,
                        index: chunks.len() as i32,
                    });
                }

                // Split the large paragraph by sentences/lines
                let sub_chunks = self.split_large_paragraph(
                    para,
                    max_tokens,
                    overlap_tokens,
                    char_offset + separator_len,
                    chunks.len() as i32,
                );
                for sub_chunk in sub_chunks {
                    chunks.push(sub_chunk);
                }

                // Reset for next paragraph with overlap from last sub-chunk
                if overlap_tokens > 0 && !chunks.is_empty() {
                    let (overlap_text, overlap_start) =
                        self.get_overlap_text(&chunks, overlap_tokens, text);
                    current_chunk = overlap_text;
                    current_tokens = self.count_tokens(&current_chunk);
                    current_start = overlap_start;
                } else {
                    current_chunk.clear();
                    current_tokens = 0;
                    current_start = char_offset + separator_len + para.len() as i32;
                }
            } else if current_tokens + para_tokens + 1 > max_tokens {
                // Adding this paragraph would exceed limit, save current and start new
                if !current_chunk.is_empty() {
                    let chunk_end = char_offset;
                    chunks.push(TextChunk {
                        content: current_chunk.clone(),
                        char_start: current_start,
                        char_end: chunk_end,
                        token_count: current_tokens as i32,
                        index: chunks.len() as i32,
                    });
                }

                // Start new chunk with overlap
                if overlap_tokens > 0 && !chunks.is_empty() {
                    let (overlap_text, overlap_start) =
                        self.get_overlap_text(&chunks, overlap_tokens, text);
                    current_chunk = overlap_text;
                    if !current_chunk.is_empty() {
                        current_chunk.push_str("\n\n");
                    }
                    current_chunk.push_str(para);
                    current_tokens = self.count_tokens(&current_chunk);
                    current_start = overlap_start;
                } else {
                    current_chunk = para.to_string();
                    current_tokens = para_tokens;
                    current_start = char_offset + separator_len;
                }
            } else {
                // Add paragraph to current chunk
                if !current_chunk.is_empty() {
                    current_chunk.push_str("\n\n");
                    current_tokens += 1; // Account for separator tokens
                }
                current_chunk.push_str(para);
                current_tokens += para_tokens;
            }

            char_offset += separator_len + para.len() as i32;
        }

        // Don't forget the last chunk
        if !current_chunk.is_empty() {
            chunks.push(TextChunk {
                content: current_chunk.clone(),
                char_start: current_start,
                char_end: text.len() as i32,
                token_count: current_tokens as i32,
                index: chunks.len() as i32,
            });
        }

        // Re-index chunks
        for (i, chunk) in chunks.iter_mut().enumerate() {
            chunk.index = i as i32;
        }

        Ok(chunks)
    }

    /// Count tokens in text using the tokenizer.
    pub fn count_tokens(&self, text: &str) -> usize {
        self.tokenizer.encode_with_special_tokens(text).len()
    }

    /// Split a large paragraph into smaller chunks.
    fn split_large_paragraph(
        &self,
        text: &str,
        max_tokens: usize,
        overlap_tokens: usize,
        base_offset: i32,
        base_index: i32,
    ) -> Vec<TextChunk> {
        let mut chunks = Vec::new();
        let sentences = split_into_sentences(text);

        let mut current_chunk = String::new();
        let mut current_tokens = 0usize;
        let mut current_start = base_offset;
        let mut char_offset = 0i32;

        for sentence in &sentences {
            let sentence_tokens = self.count_tokens(sentence);

            if sentence_tokens > max_tokens {
                // Sentence itself is too large, split by words
                if !current_chunk.is_empty() {
                    chunks.push(TextChunk {
                        content: current_chunk.clone(),
                        char_start: current_start,
                        char_end: base_offset + char_offset,
                        token_count: current_tokens as i32,
                        index: base_index + chunks.len() as i32,
                    });
                    current_chunk.clear();
                    current_tokens = 0;
                    current_start = base_offset + char_offset;
                }

                // Split sentence by words
                let words: Vec<&str> = sentence.split_whitespace().collect();
                for word in words {
                    let word_tokens = self.count_tokens(word);
                    if current_tokens + word_tokens + 1 > max_tokens && !current_chunk.is_empty() {
                        chunks.push(TextChunk {
                            content: current_chunk.clone(),
                            char_start: current_start,
                            char_end: base_offset + char_offset,
                            token_count: current_tokens as i32,
                            index: base_index + chunks.len() as i32,
                        });

                        // Add overlap from previous chunk
                        if overlap_tokens > 0 && !chunks.is_empty() {
                            let overlap_words = get_last_n_tokens_worth(
                                &current_chunk,
                                overlap_tokens,
                                &self.tokenizer,
                            );
                            current_chunk = overlap_words;
                            current_tokens = self.count_tokens(&current_chunk);
                        } else {
                            current_chunk.clear();
                            current_tokens = 0;
                        }
                        current_start = base_offset + char_offset;
                    }

                    if !current_chunk.is_empty() {
                        current_chunk.push(' ');
                        current_tokens += 1;
                    }
                    current_chunk.push_str(word);
                    current_tokens += word_tokens;
                }
            } else if current_tokens + sentence_tokens + 1 > max_tokens {
                // Adding sentence would exceed limit
                if !current_chunk.is_empty() {
                    chunks.push(TextChunk {
                        content: current_chunk.clone(),
                        char_start: current_start,
                        char_end: base_offset + char_offset,
                        token_count: current_tokens as i32,
                        index: base_index + chunks.len() as i32,
                    });
                }

                // Start new chunk with overlap
                if overlap_tokens > 0 && !chunks.is_empty() {
                    let overlap_words =
                        get_last_n_tokens_worth(&current_chunk, overlap_tokens, &self.tokenizer);
                    current_chunk = format!("{} {}", overlap_words, sentence);
                    current_tokens = self.count_tokens(&current_chunk);
                } else {
                    current_chunk = sentence.to_string();
                    current_tokens = sentence_tokens;
                }
                current_start = base_offset + char_offset;
            } else {
                // Add sentence to current chunk
                if !current_chunk.is_empty() {
                    current_chunk.push(' ');
                    current_tokens += 1;
                }
                current_chunk.push_str(sentence);
                current_tokens += sentence_tokens;
            }

            char_offset += sentence.len() as i32 + 1; // +1 for space between sentences
        }

        // Last chunk
        if !current_chunk.is_empty() {
            chunks.push(TextChunk {
                content: current_chunk.clone(),
                char_start: current_start,
                char_end: base_offset + text.len() as i32,
                token_count: current_tokens as i32,
                index: base_index + chunks.len() as i32,
            });
        }

        chunks
    }

    /// Get overlap text from previous chunks.
    fn get_overlap_text(
        &self,
        chunks: &[TextChunk],
        target_tokens: usize,
        _text: &str,
    ) -> (String, i32) {
        if chunks.is_empty() {
            return (String::new(), 0);
        }

        let last_chunk = chunks
            .last()
            .expect("chunks guaranteed non-empty by preceding is_empty check");
        let overlap_text =
            get_last_n_tokens_worth(&last_chunk.content, target_tokens, &self.tokenizer);
        let overlap_start = last_chunk.char_end - overlap_text.len() as i32;

        (overlap_text, overlap_start.max(0))
    }

    /// Generate embedding and store chunk with embedding in vector store.
    ///
    /// # Arguments
    /// * `embedding_service` - Service for generating embeddings
    /// * `vector_store` - Vector store for storing chunks
    /// * `vector_store_id` - The vector store this chunk belongs to
    /// * `file_id` - The file this chunk was extracted from
    /// * `chunk` - The text chunk to embed and store
    /// * `file_attributes` - Optional attributes from the file to include in chunk metadata
    /// * `processing_version` - Shared version UUID for atomic shadow-copy updates
    #[allow(clippy::too_many_arguments)]
    #[instrument(
        skip(self, embedding_service, vector_store, file_attributes),
        fields(
            chunk_index = chunk.index,
            chunk_tokens = chunk.token_count
        )
    )]
    async fn generate_and_store_chunk(
        &self,
        embedding_service: &EmbeddingService,
        vector_store: &dyn VectorBackend,
        vector_store_id: Uuid,
        file_id: Uuid,
        chunk: TextChunk,
        file_attributes: Option<&std::collections::HashMap<String, serde_json::Value>>,
        processing_version: Uuid,
    ) -> Result<(), DocumentProcessorError> {
        // Generate embedding span
        let embed_span = info_span!("generate_embedding", content_len = chunk.content.len());
        let embedding = match async { embedding_service.embed_text(&chunk.content).await }
            .instrument(embed_span)
            .await
        {
            Ok(emb) => emb,
            Err(e) => {
                otel_span_error!("Embedding generation failed: {}", e);
                return Err(e.into());
            }
        };

        // Create unique ID for this chunk using deterministic UUID v5
        let chunk_id_str = format!(
            "col:{}:file:{}:chunk:{}",
            vector_store_id, file_id, chunk.index
        );
        let chunk_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, chunk_id_str.as_bytes());

        // Convert file attributes to chunk metadata
        // The attributes are stored as-is in the metadata field, allowing
        // attribute-based filtering during search operations.
        let metadata = file_attributes.map(|attrs| serde_json::json!(attrs));

        // Create chunk with embedding for storage - move content to avoid clone
        // All chunks from the same processing run share the same processing_version,
        // enabling atomic shadow-copy cleanup after successful storage.
        let chunk_with_embedding = crate::cache::vector_store::ChunkWithEmbedding {
            id: chunk_id,
            vector_store_id,
            file_id,
            chunk_index: chunk.index,
            content: chunk.content,
            token_count: chunk.token_count,
            char_start: chunk.char_start,
            char_end: chunk.char_end,
            embedding,
            metadata,
            processing_version,
        };

        // Store chunk span - wraps the circuit breaker and retry logic
        let store_span = info_span!("store_chunk", chunk_id = %chunk_id);
        let store_result = async {
            with_circuit_breaker_and_retry_generic(
                self.circuit_breaker.as_deref(),
                &self.config.retry,
                "vector_store",
                "store_chunk",
                |e: &crate::cache::vector_store::VectorStoreError| {
                    // Retry on database errors that look transient
                    match e {
                        crate::cache::vector_store::VectorStoreError::Database(msg) => {
                            is_retryable_database_error(msg)
                        }
                        crate::cache::vector_store::VectorStoreError::Http(_) => true, // Qdrant HTTP errors
                        _ => false, // Don't retry dimension mismatches, etc.
                    }
                },
                |_| false, // Successful stores are never failures for circuit breaker
                || {
                    let chunk = chunk_with_embedding.clone();
                    async move { vector_store.store_chunks(vec![chunk]).await }
                },
            )
            .await
            .map_err(|e| match e {
                crate::providers::retry::GenericRequestError::CircuitBreakerOpen(cb_err) => {
                    DocumentProcessorError::CircuitBreakerOpen(cb_err.to_string())
                }
                crate::providers::retry::GenericRequestError::Operation(op_err) => {
                    DocumentProcessorError::VectorStore(op_err.to_string())
                }
            })
        }
        .instrument(store_span)
        .await;

        match store_result {
            Ok(()) => {
                otel_span_ok!();
                Ok(())
            }
            Err(e) => {
                otel_span_error!("Chunk storage failed: {}", e);
                Err(e)
            }
        }
    }

    /// Update vector store file status with error.
    async fn update_file_error(
        &self,
        vector_store_file_id: Uuid,
        code: FileErrorCode,
        message: &str,
    ) -> Result<(), DocumentProcessorError> {
        error!(vector_store_file_id = %vector_store_file_id, code = code.as_str(), message, "File processing failed");

        self.vector_stores_service
            .update_vector_store_file_status(
                vector_store_file_id,
                VectorStoreFileStatus::Failed,
                Some(FileError {
                    code,
                    message: message.to_string(),
                }),
            )
            .await?;

        Ok(())
    }

    /// Process a file in the background.
    ///
    /// Spawns a task that processes the file without blocking.
    /// Status updates are written to the database.
    /// The current span context is propagated to the spawned task for distributed tracing.
    pub fn process_file_background(self: Arc<Self>, file_id: Uuid) {
        // Create a span for background processing and propagate trace context
        let span = info_span!(
            "process_file_background",
            file_id = %file_id,
        );

        tokio::spawn(
            async move {
                if let Err(e) = self.process_file(file_id).await {
                    error!(file_id = %file_id, error = %e, "Background file processing failed");
                }
            }
            .instrument(span),
        );
    }

    /// Schedule a file for processing based on the configured processing mode.
    ///
    /// - In `Inline` mode: Processes the file in a background task
    /// - In `Queue` mode: Publishes a job to the configured queue
    #[instrument(skip(self), fields(file_id = %file_id))]
    pub async fn schedule_processing(
        self: Arc<Self>,
        file_id: Uuid,
    ) -> Result<(), DocumentProcessorError> {
        let result = match self.config.processing_mode {
            ProcessingMode::Inline => {
                info!("Scheduling inline processing");
                self.process_file_background(file_id);
                Ok(())
            }
            ProcessingMode::Queue => {
                info!("Publishing to processing queue");
                self.publish_processing_job(file_id).await
            }
        };

        match &result {
            Ok(()) => {
                otel_span_ok!();
            }
            Err(_e) => {
                otel_span_error!("Scheduling failed: {}", _e);
            }
        }
        result
    }

    /// Publish a processing job to the configured queue.
    ///
    /// This is used when `processing_mode` is set to `Queue`.
    /// External workers consume jobs from the queue and process files.
    #[instrument(skip(self), fields(vector_store_file_id = %vector_store_file_id))]
    pub async fn publish_processing_job(
        &self,
        vector_store_file_id: Uuid,
    ) -> Result<(), DocumentProcessorError> {
        // Get vector store file link
        let vector_store_file = match self
            .vector_stores_service
            .get_vector_store_file(vector_store_file_id)
            .await
        {
            Ok(Some(cf)) => cf,
            Ok(None) => {
                otel_span_error!("VectorStore file not found");
                return Err(DocumentProcessorError::FileNotFound(vector_store_file_id));
            }
            Err(e) => {
                otel_span_error!("Database error: {}", e);
                return Err(e.into());
            }
        };

        // Get actual file info from Files API for storage details
        let file = match self.db.files().get_file(vector_store_file.file_id).await {
            Ok(Some(f)) => f,
            Ok(None) => {
                otel_span_error!("File not found");
                return Err(DocumentProcessorError::FileNotFound(
                    vector_store_file.file_id,
                ));
            }
            Err(e) => {
                otel_span_error!("Database error: {}", e);
                return Err(e.into());
            }
        };

        let job = ProcessingJob {
            job_id: Uuid::new_v4(),
            file_id: vector_store_file.file_id,
            vector_store_id: vector_store_file.vector_store_id,
            storage_backend: file.storage_backend.as_str().to_string(),
            storage_path: file.storage_path,
            chunking_strategy: vector_store_file.chunking_strategy,
            callback_url: self.config.callback_url.clone(),
        };

        let job_json = serde_json::to_string(&job).map_err(|e| {
            DocumentProcessorError::Database(format!("Failed to serialize job: {}", e))
        })?;

        // Publish to queue based on backend configuration
        match &self.config.queue_backend {
            #[cfg(feature = "redis")]
            Some(QueueBackend::Redis {
                url,
                queue_name,
                consumer_group,
            }) => {
                debug!(url, queue_name, job_id = %job.job_id, "Publishing to Redis queue");

                // Create Redis client and connection
                let client = redis::Client::open(url.as_str()).map_err(|e| {
                    DocumentProcessorError::Database(format!(
                        "Failed to create Redis client: {}",
                        e
                    ))
                })?;

                let mut conn = client
                    .get_multiplexed_async_connection()
                    .await
                    .map_err(|e| {
                        DocumentProcessorError::Database(format!(
                            "Failed to connect to Redis: {}",
                            e
                        ))
                    })?;

                // Ensure consumer group exists (MKSTREAM creates the stream if needed)
                // This is idempotent - BUSYGROUP error means it already exists
                let group_result: redis::RedisResult<()> = redis::cmd("XGROUP")
                    .arg("CREATE")
                    .arg(queue_name)
                    .arg(consumer_group)
                    .arg("0")
                    .arg("MKSTREAM")
                    .query_async(&mut conn)
                    .await;

                // Ignore BUSYGROUP error (group already exists)
                if let Err(e) = group_result {
                    let err_str = e.to_string();
                    if !err_str.contains("BUSYGROUP") {
                        otel_span_error!("Failed to create consumer group");
                        return Err(DocumentProcessorError::Database(format!(
                            "Failed to create consumer group: {}",
                            e
                        )));
                    }
                }

                // Publish job to stream using XADD
                // Use "*" for auto-generated ID (timestamp-based)
                let stream_id: String = redis::cmd("XADD")
                    .arg(queue_name)
                    .arg("*")
                    .arg("job_id")
                    .arg(job.job_id.to_string())
                    .arg("file_id")
                    .arg(job.file_id.to_string())
                    .arg("vector_store_id")
                    .arg(job.vector_store_id.to_string())
                    .arg("data")
                    .arg(&job_json)
                    .query_async(&mut conn)
                    .await
                    .map_err(|e| {
                        DocumentProcessorError::Database(format!(
                            "Failed to publish to Redis stream: {}",
                            e
                        ))
                    })?;

                info!(
                    job_id = %job.job_id,
                    stream_id = %stream_id,
                    queue_name = %queue_name,
                    "Job published to Redis stream"
                );
            }
            #[cfg(not(feature = "redis"))]
            Some(QueueBackend::Redis { .. }) => {
                return Err(DocumentProcessorError::Configuration(
                    "Redis queue configured but the 'redis' feature is not enabled. \
                     Rebuild with: cargo build --features redis"
                        .to_string(),
                ));
            }
            None => {
                error!("Queue mode enabled but no queue backend configured");
                otel_span_error!("Queue backend not configured");
                return Err(DocumentProcessorError::Configuration(
                    "Queue backend not configured".to_string(),
                ));
            }
        }

        info!(job_id = %job.job_id, "Processing job published (job_json length: {})", job_json.len());
        otel_span_ok!();
        Ok(())
    }

    /// Get the current processing mode.
    pub fn processing_mode(&self) -> &ProcessingMode {
        &self.config.processing_mode
    }

    /// Get the queue backend configuration.
    pub fn queue_backend(&self) -> Option<&QueueBackend> {
        self.config.queue_backend.as_ref()
    }
}

// ============================================================================
// Worker Module
// ============================================================================

/// Configuration for the file processing worker.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Consumer name (unique identifier for this worker instance).
    pub consumer_name: String,
    /// Block timeout in milliseconds when waiting for new jobs.
    pub block_timeout_ms: u64,
    /// Number of messages to read per batch.
    pub batch_size: usize,
    /// Interval in seconds between idle checks when no jobs are available.
    pub idle_interval_secs: u64,
    /// Whether to claim pending messages from other consumers on startup.
    pub claim_pending: bool,
    /// Maximum idle time in milliseconds before a pending message can be claimed.
    pub pending_timeout_ms: u64,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            consumer_name: format!("worker-{}", uuid::Uuid::new_v4()),
            block_timeout_ms: 5000,
            batch_size: 10,
            idle_interval_secs: 1,
            claim_pending: true,
            pending_timeout_ms: 60_000, // 1 minute
        }
    }
}

/// Result of processing a single job.
#[derive(Debug)]
pub struct JobResult {
    pub job_id: Uuid,
    pub stream_id: String,
    pub success: bool,
    pub chunks_created: Option<usize>,
    pub error: Option<String>,
}

/// Starts the file processing worker as a background task.
///
/// The worker consumes jobs from a Redis Stream and processes them using
/// the provided DocumentProcessor. It runs in a loop until cancelled.
///
/// # Arguments
/// * `processor` - The DocumentProcessor to use for file processing
/// * `worker_config` - Worker-specific configuration
///
/// # Queue Backend
/// Currently only Redis Streams is implemented. The processor must be
/// configured with a Redis queue backend.
pub async fn start_file_processing_worker(
    processor: Arc<DocumentProcessor>,
    worker_config: WorkerConfig,
) {
    let _ = &worker_config; // Used by redis feature
    let queue_backend = match processor.queue_backend() {
        Some(backend) => backend.clone(),
        None => {
            tracing::error!("File processing worker requires queue backend configuration");
            return;
        }
    };

    match queue_backend {
        #[cfg(feature = "redis")]
        QueueBackend::Redis {
            url,
            queue_name,
            consumer_group,
        } => {
            start_redis_worker(processor, &url, &queue_name, &consumer_group, worker_config).await;
        }
        #[cfg(not(feature = "redis"))]
        QueueBackend::Redis { .. } => {
            tracing::error!(
                "Redis queue configured but the 'redis' feature is not enabled. Rebuild with: cargo build --features redis"
            );
        }
    }
}

#[cfg(feature = "redis")]
/// Start the Redis Streams worker loop.
async fn start_redis_worker(
    processor: Arc<DocumentProcessor>,
    url: &str,
    queue_name: &str,
    consumer_group: &str,
    config: WorkerConfig,
) {
    tracing::info!(
        queue_name = queue_name,
        consumer_group = consumer_group,
        consumer_name = config.consumer_name,
        batch_size = config.batch_size,
        "Starting Redis file processing worker"
    );

    // Create Redis client
    let client = match redis::Client::open(url) {
        Ok(client) => client,
        Err(e) => {
            tracing::error!(error = %e, "Failed to create Redis client");
            return;
        }
    };

    let mut conn = match client.get_multiplexed_async_connection().await {
        Ok(conn) => conn,
        Err(e) => {
            tracing::error!(error = %e, "Failed to connect to Redis");
            return;
        }
    };

    // Ensure consumer group exists
    let group_result: redis::RedisResult<()> = redis::cmd("XGROUP")
        .arg("CREATE")
        .arg(queue_name)
        .arg(consumer_group)
        .arg("0")
        .arg("MKSTREAM")
        .query_async(&mut conn)
        .await;

    if let Err(e) = group_result {
        let err_str = e.to_string();
        if !err_str.contains("BUSYGROUP") {
            tracing::error!(error = %e, "Failed to create consumer group");
            return;
        }
        // BUSYGROUP means group already exists, which is fine
    }

    // Optionally claim pending messages from other consumers
    if config.claim_pending {
        claim_pending_messages(
            &mut conn,
            queue_name,
            consumer_group,
            &config.consumer_name,
            config.pending_timeout_ms,
            &processor,
        )
        .await;
    }

    // Main worker loop
    loop {
        match read_and_process_jobs(
            &mut conn,
            queue_name,
            consumer_group,
            &config.consumer_name,
            config.batch_size,
            config.block_timeout_ms,
            &processor,
        )
        .await
        {
            Ok(0) => {
                // No jobs available, sleep before checking again
                tokio::time::sleep(std::time::Duration::from_secs(config.idle_interval_secs)).await;
            }
            Ok(processed) => {
                tracing::debug!(processed = processed, "Processed batch of jobs");
            }
            Err(e) => {
                tracing::error!(error = %e, "Error reading from Redis stream");
                // Sleep before retrying on error
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    }
}

#[cfg(feature = "redis")]
/// Read and process jobs from the Redis stream.
///
/// Returns the number of jobs processed.
async fn read_and_process_jobs(
    conn: &mut redis::aio::MultiplexedConnection,
    queue_name: &str,
    consumer_group: &str,
    consumer_name: &str,
    batch_size: usize,
    block_timeout_ms: u64,
    processor: &DocumentProcessor,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    // XREADGROUP GROUP <group> <consumer> COUNT <count> BLOCK <ms> STREAMS <key> >
    // The ">" means only read new messages not yet delivered to any consumer
    let result: redis::Value = redis::cmd("XREADGROUP")
        .arg("GROUP")
        .arg(consumer_group)
        .arg(consumer_name)
        .arg("COUNT")
        .arg(batch_size)
        .arg("BLOCK")
        .arg(block_timeout_ms)
        .arg("STREAMS")
        .arg(queue_name)
        .arg(">")
        .query_async(conn)
        .await?;

    // Parse the response and process jobs
    let jobs = parse_stream_response(result)?;

    if jobs.is_empty() {
        return Ok(0);
    }

    let mut processed = 0;
    for (stream_id, job) in jobs {
        let result = process_single_job(processor, &job).await;

        // Log result
        if result.success {
            tracing::info!(
                job_id = %result.job_id,
                stream_id = %result.stream_id,
                chunks_created = result.chunks_created,
                "Job completed successfully"
            );
        } else {
            tracing::error!(
                job_id = %result.job_id,
                stream_id = %result.stream_id,
                error = ?result.error,
                "Job failed"
            );
        }

        // Acknowledge the message regardless of success/failure
        // (failed jobs have their status stored in the database)
        let ack_result: redis::RedisResult<i64> = redis::cmd("XACK")
            .arg(queue_name)
            .arg(consumer_group)
            .arg(&stream_id)
            .query_async(conn)
            .await;

        if let Err(e) = ack_result {
            tracing::warn!(
                stream_id = stream_id,
                error = %e,
                "Failed to acknowledge message"
            );
        }

        // Send callback if configured
        if let Some(callback_url) = &job.callback_url {
            send_callback(callback_url, &result).await;
        }

        processed += 1;
    }

    Ok(processed)
}

#[cfg(feature = "redis")]
/// Parse the Redis XREADGROUP response into jobs.
fn parse_stream_response(
    response: redis::Value,
) -> Result<Vec<(String, ProcessingJob)>, Box<dyn std::error::Error + Send + Sync>> {
    let mut jobs = Vec::new();

    // Response format: [[stream_name, [[id, [field, value, ...]]]]]
    let streams = match response {
        redis::Value::Array(streams) => streams,
        redis::Value::Nil => return Ok(jobs),
        _ => return Err("Unexpected response format".into()),
    };

    for stream in streams {
        let stream_data = match stream {
            redis::Value::Array(data) => data,
            _ => continue,
        };

        // Second element is the array of messages
        if stream_data.len() < 2 {
            continue;
        }

        let messages = match &stream_data[1] {
            redis::Value::Array(msgs) => msgs,
            _ => continue,
        };

        for message in messages {
            let msg_data = match message {
                redis::Value::Array(data) => data,
                _ => continue,
            };

            if msg_data.len() < 2 {
                continue;
            }

            // First element is the stream ID
            let stream_id = match &msg_data[0] {
                redis::Value::BulkString(id) => String::from_utf8_lossy(id).to_string(),
                _ => continue,
            };

            // Second element is the field array
            let fields = match &msg_data[1] {
                redis::Value::Array(fields) => fields,
                _ => continue,
            };

            // Find the "data" field containing the serialized job
            let mut iter = fields.iter();
            while let (Some(key), Some(val)) = (iter.next(), iter.next()) {
                if let redis::Value::BulkString(k) = key
                    && k == b"data"
                    && let redis::Value::BulkString(v) = val
                {
                    match serde_json::from_slice::<ProcessingJob>(v) {
                        Ok(job) => jobs.push((stream_id.clone(), job)),
                        Err(e) => {
                            tracing::warn!(
                                stream_id = stream_id,
                                error = %e,
                                "Failed to deserialize job"
                            );
                        }
                    }
                }
            }
        }
    }

    Ok(jobs)
}

#[cfg(feature = "redis")]
/// Process a single job.
async fn process_single_job(processor: &DocumentProcessor, job: &ProcessingJob) -> JobResult {
    tracing::info!(
        job_id = %job.job_id,
        file_id = %job.file_id,
        vector_store_id = %job.vector_store_id,
        "Processing job"
    );

    // The job contains the file_id, but we need to find the vector_store_file_id
    // which is the link between the file and the vector_store.
    // For now, we use file_id directly since process_file expects vector_store_file_id
    // This assumes the caller passed the correct ID.

    match processor.process_file(job.file_id).await {
        Ok(chunks_created) => JobResult {
            job_id: job.job_id,
            stream_id: String::new(), // Set by caller
            success: true,
            chunks_created: Some(chunks_created),
            error: None,
        },
        Err(e) => JobResult {
            job_id: job.job_id,
            stream_id: String::new(),
            success: false,
            chunks_created: None,
            error: Some(e.to_string()),
        },
    }
}

#[cfg(feature = "redis")]
/// Claim pending messages from other consumers that have timed out.
async fn claim_pending_messages(
    conn: &mut redis::aio::MultiplexedConnection,
    queue_name: &str,
    consumer_group: &str,
    consumer_name: &str,
    min_idle_ms: u64,
    processor: &DocumentProcessor,
) {
    tracing::debug!(
        min_idle_ms = min_idle_ms,
        "Checking for pending messages to claim"
    );

    // XPENDING to get pending messages
    let pending_result: redis::RedisResult<redis::Value> = redis::cmd("XPENDING")
        .arg(queue_name)
        .arg(consumer_group)
        .arg("-")
        .arg("+")
        .arg(100) // Max 100 pending messages
        .query_async(conn)
        .await;

    let pending_entries = match pending_result {
        Ok(redis::Value::Array(entries)) => entries,
        Ok(_) => return,
        Err(e) => {
            tracing::debug!(error = %e, "No pending messages or error checking");
            return;
        }
    };

    let mut claimed_ids = Vec::new();

    for entry in pending_entries {
        // Each entry is [message_id, consumer_name, idle_time, delivery_count]
        let entry_data = match entry {
            redis::Value::Array(data) if data.len() >= 3 => data,
            _ => continue,
        };

        let message_id = match &entry_data[0] {
            redis::Value::BulkString(id) => String::from_utf8_lossy(id).to_string(),
            _ => continue,
        };

        let idle_time = match &entry_data[2] {
            redis::Value::Int(ms) => *ms as u64,
            _ => continue,
        };

        if idle_time >= min_idle_ms {
            claimed_ids.push(message_id);
        }
    }

    if claimed_ids.is_empty() {
        return;
    }

    tracing::info!(
        count = claimed_ids.len(),
        "Claiming pending messages from other consumers"
    );

    // XCLAIM to take ownership of the messages
    for message_id in &claimed_ids {
        let claim_result: redis::RedisResult<redis::Value> = redis::cmd("XCLAIM")
            .arg(queue_name)
            .arg(consumer_group)
            .arg(consumer_name)
            .arg(min_idle_ms)
            .arg(message_id)
            .query_async(conn)
            .await;

        match claim_result {
            Ok(redis::Value::Array(messages)) if !messages.is_empty() => {
                // Parse and process the claimed message
                if let Ok(jobs) =
                    parse_stream_response(redis::Value::Array(vec![redis::Value::Array(vec![
                        redis::Value::BulkString(queue_name.as_bytes().to_vec()),
                        redis::Value::Array(messages),
                    ])]))
                {
                    for (_stream_id, job) in jobs {
                        let result = process_single_job(processor, &job).await;
                        if result.success {
                            // Acknowledge successful processing
                            let _: redis::RedisResult<i64> = redis::cmd("XACK")
                                .arg(queue_name)
                                .arg(consumer_group)
                                .arg(message_id)
                                .query_async(conn)
                                .await;
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(message_id = message_id, error = %e, "Failed to claim message");
            }
            _ => {}
        }
    }
}

#[cfg(feature = "redis")]
/// Send a callback notification for job completion.
async fn send_callback(callback_url: &str, result: &JobResult) {
    let client = reqwest::Client::new();

    let payload = serde_json::json!({
        "job_id": result.job_id,
        "success": result.success,
        "chunks_created": result.chunks_created,
        "error": result.error,
    });

    match client.post(callback_url).json(&payload).send().await {
        Ok(response) => {
            if response.status().is_success() {
                tracing::debug!(
                    job_id = %result.job_id,
                    callback_url = callback_url,
                    "Callback sent successfully"
                );
            } else {
                tracing::warn!(
                    job_id = %result.job_id,
                    callback_url = callback_url,
                    status = %response.status(),
                    "Callback returned non-success status"
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                job_id = %result.job_id,
                callback_url = callback_url,
                error = %e,
                "Failed to send callback"
            );
        }
    }
}

impl std::fmt::Debug for DocumentProcessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocumentProcessor")
            .field("config", &self.config)
            .field("has_embedding_service", &self.embedding_service.is_some())
            .field("has_vector_store", &self.vector_store.is_some())
            .finish()
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Get file extension from filename.
fn get_file_extension(filename: &str) -> String {
    filename.rsplit('.').next().unwrap_or("").to_lowercase()
}

/// Check if a file type is supported.
///
/// Supported formats fall into three categories:
/// 1. **Plain text** - Direct UTF-8 text files (txt, md, json, csv, code files, etc.)
/// 2. **Rich documents** - Extracted via xberg (PDF, Office, OpenDocument, EPUB, RTF)
/// 3. **Images with OCR** - Requires `ocr` feature in xberg (not yet enabled)
fn is_supported_file_type(extension: &str) -> bool {
    matches!(
        extension,
        // Plain text
        "txt" | "md" | "markdown" | "json" | "csv" | "xml" | "html" | "htm" |
        // Code files
        "rs" | "py" | "js" | "ts" | "jsx" | "tsx" | "go" | "java" | "c" | "cpp" |
        "h" | "hpp" | "cs" | "rb" | "php" | "swift" | "kt" | "scala" | "r" |
        "sql" | "sh" | "bash" | "zsh" | "ps1" | "yaml" | "yml" | "toml" | "ini" |
        "cfg" | "conf" | "properties" | "env" | "dockerfile" | "makefile" |
        // Documentation
        "rst" | "adoc" | "tex" | "latex" |
        // Rich documents (extracted via xberg)
        "pdf" |                                    // PDF documents
        "docx" | "doc" |                           // Microsoft Word
        "xlsx" | "xls" |                           // Microsoft Excel
        "pptx" | "ppt" |                           // Microsoft PowerPoint
        "odt" | "ods" | "odp" |                    // OpenDocument (Writer, Calc, Impress)
        "rtf" |                                    // Rich Text Format
        "epub" |                                   // EPUB ebooks
        // Images (OCR extraction via xberg + Tesseract)
        "png" | "jpg" | "jpeg" | "tiff" | "tif" | "bmp" | "webp" | "gif"
    )
}

/// Check if a file extension represents a plain text file.
///
/// Plain text files can be read directly as UTF-8 without document extraction.
/// Rich document formats (PDF, Office, etc.) require xberg for extraction.
fn is_plain_text_type(extension: &str) -> bool {
    matches!(
        extension,
        // Plain text
        "txt" | "md" | "markdown" | "json" | "csv" | "xml" | "html" | "htm" |
        // Code files
        "rs" | "py" | "js" | "ts" | "jsx" | "tsx" | "go" | "java" | "c" | "cpp" |
        "h" | "hpp" | "cs" | "rb" | "php" | "swift" | "kt" | "scala" | "r" |
        "sql" | "sh" | "bash" | "zsh" | "ps1" | "yaml" | "yml" | "toml" | "ini" |
        "cfg" | "conf" | "properties" | "env" | "dockerfile" | "makefile" |
        // Documentation
        "rst" | "adoc" | "tex" | "latex"
    )
}

#[cfg(any(feature = "document-extraction-full", test))]
/// Map a file extension to its MIME type for xberg extraction.
///
/// Returns the MIME type string used by xberg for document extraction.
/// Only maps rich document formats that require xberg; plain text files
/// should use direct UTF-8 conversion instead.
fn extension_to_mime(extension: &str) -> Option<&'static str> {
    match extension {
        // PDF
        "pdf" => Some("application/pdf"),
        // Microsoft Word
        "docx" => Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        "doc" => Some("application/msword"),
        // Microsoft Excel
        "xlsx" => Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
        "xls" => Some("application/vnd.ms-excel"),
        // Microsoft PowerPoint
        "pptx" => Some("application/vnd.openxmlformats-officedocument.presentationml.presentation"),
        "ppt" => Some("application/vnd.ms-powerpoint"),
        // OpenDocument
        "odt" => Some("application/vnd.oasis.opendocument.text"),
        "ods" => Some("application/vnd.oasis.opendocument.spreadsheet"),
        "odp" => Some("application/vnd.oasis.opendocument.presentation"),
        // Other rich formats
        "rtf" => Some("application/rtf"),
        "epub" => Some("application/epub+zip"),
        // Images (for OCR)
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "tiff" | "tif" => Some("image/tiff"),
        "bmp" => Some("image/bmp"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

/// Extract text content from file data.
///
/// For plain text files (code, markdown, JSON, etc.), performs direct UTF-8 conversion.
/// For rich documents (PDF, Office, EPUB, etc.), uses xberg for extraction.
///
/// Takes ownership of the byte vector to avoid copying when possible.
///
/// # Arguments
/// * `data` - File content as bytes
/// * `extension` - File extension (without leading dot)
/// * `extraction_config` - Document extraction configuration (OCR, PDF options)
async fn extract_text(
    data: Vec<u8>,
    extension: &str,
    extraction_config: &DocumentExtractionConfig,
) -> Result<String, DocumentProcessorError> {
    // Plain text files: direct UTF-8 conversion (fast path)
    if is_plain_text_type(extension) {
        return String::from_utf8(data).map_err(|_| DocumentProcessorError::InvalidUtf8);
    }

    // Rich documents: use xberg for extraction (requires document-extraction-full feature)
    #[cfg(feature = "document-extraction-full")]
    {
        let mime_type = extension_to_mime(extension).ok_or_else(|| {
            DocumentProcessorError::UnsupportedFileType(format!(
                "No MIME type mapping for extension: {}",
                extension
            ))
        })?;

        // Build xberg extraction config from our config
        let config = build_xberg_config(extraction_config);
        let input = xberg::ExtractInput::from_bytes(data, mime_type, None);
        let extraction = xberg::extract(input, &config);

        // Bound how long any single document may tie up an extraction worker.
        // xberg has no internal hard limit, so a 5,000-page OCR job (or a
        // pathological/malicious input) would otherwise run unbounded.
        let result = if extraction_config.extraction_timeout_secs > 0 {
            let timeout = std::time::Duration::from_secs(extraction_config.extraction_timeout_secs);
            match tokio::time::timeout(timeout, extraction).await {
                Ok(r) => r,
                Err(_) => {
                    return Err(DocumentProcessorError::DocumentExtraction(format!(
                        "Document extraction exceeded {}s timeout",
                        extraction_config.extraction_timeout_secs
                    )));
                }
            }
        } else {
            extraction.await
        }
        .map_err(|e| DocumentProcessorError::DocumentExtraction(e.to_string()))?;

        // xberg returns an envelope of per-document results; a single-input
        // `extract` yields exactly one on success.
        result
            .results
            .into_iter()
            .next()
            .map(|document| document.content)
            .ok_or_else(|| {
                DocumentProcessorError::DocumentExtraction(
                    "Document extraction produced no results".to_string(),
                )
            })
    }

    #[cfg(not(feature = "document-extraction-full"))]
    {
        // Suppress unused variable warnings when feature is disabled
        let _ = (data, extraction_config);
        Err(DocumentProcessorError::UnsupportedFileType(format!(
            "Rich document extraction for '.{}' files requires the 'document-extraction-full' feature. \
            Rebuild with: cargo build --features document-extraction-full",
            extension
        )))
    }
}

/// Build an xberg ExtractionConfig from our DocumentExtractionConfig.
#[cfg(feature = "document-extraction-full")]
fn build_xberg_config(config: &DocumentExtractionConfig) -> xberg::ExtractionConfig {
    let mut xberg_config = xberg::ExtractionConfig::default();

    // Configure OCR if enabled
    if config.enable_ocr {
        xberg_config.ocr = Some(xberg::OcrConfig {
            backend: "tesseract".to_string(),
            language: vec![config.ocr_language.clone()],
            ..Default::default()
        });
        xberg_config.force_ocr = config.force_ocr;
    }

    // Configure PDF-specific options
    xberg_config.pdf_options = Some(xberg::PdfConfig {
        extract_images: config.pdf_extract_images,
        extract_metadata: true,
        ..Default::default()
    });

    // Configure image extraction settings (includes DPI for OCR)
    if config.pdf_extract_images || config.enable_ocr {
        xberg_config.images = Some(xberg::ImageExtractionConfig {
            extract_images: config.pdf_extract_images,
            target_dpi: config.pdf_image_dpi as i32,
            max_image_dimension: 4096,
            auto_adjust_dpi: true,
            min_dpi: 72,
            max_dpi: 600,
            ..Default::default()
        });
    }

    xberg_config
}

/// Split text into sentences (simple heuristic).
fn split_into_sentences(text: &str) -> Vec<&str> {
    // Simple sentence splitting - could be improved with NLP libraries
    let mut sentences = Vec::new();
    let mut last = 0;

    for (i, c) in text.char_indices() {
        if c == '.' || c == '!' || c == '?' {
            // Check if followed by space or end of string
            let next_idx = i + c.len_utf8();
            if next_idx >= text.len()
                || text[next_idx..].starts_with(' ')
                || text[next_idx..].starts_with('\n')
            {
                let sentence = text[last..=i].trim();
                if !sentence.is_empty() {
                    sentences.push(sentence);
                }
                last = next_idx;
            }
        }
    }

    // Don't forget remaining text
    if last < text.len() {
        let remaining = text[last..].trim();
        if !remaining.is_empty() {
            sentences.push(remaining);
        }
    }

    // If no sentences found, return the whole text
    if sentences.is_empty() && !text.is_empty() {
        sentences.push(text.trim());
    }

    sentences
}

/// Get the last N tokens worth of text from a string.
fn get_last_n_tokens_worth(text: &str, target_tokens: usize, tokenizer: &CoreBPE) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return String::new();
    }

    // Start from the end and work backwards
    let mut result_words = Vec::new();
    let mut token_count = 0;

    for word in words.iter().rev() {
        let word_tokens = tokenizer.encode_with_special_tokens(word).len();
        if token_count + word_tokens > target_tokens && !result_words.is_empty() {
            break;
        }
        result_words.push(*word);
        token_count += word_tokens;
    }

    result_words.reverse();
    result_words.join(" ")
}

#[cfg(test)]
mod tests {
    use serial_test::serial;

    use super::*;

    fn create_test_tokenizer() -> CoreBPE {
        cl100k_base().unwrap()
    }

    #[test]
    fn test_get_file_extension() {
        assert_eq!(get_file_extension("test.txt"), "txt");
        assert_eq!(get_file_extension("test.TXT"), "txt");
        assert_eq!(get_file_extension("path/to/file.md"), "md");
        assert_eq!(get_file_extension("noextension"), "noextension");
        assert_eq!(get_file_extension(".hidden"), "hidden");
    }

    #[test]
    fn test_is_supported_file_type() {
        // Plain text types
        assert!(is_supported_file_type("txt"));
        assert!(is_supported_file_type("md"));
        assert!(is_supported_file_type("json"));
        assert!(is_supported_file_type("csv"));
        assert!(is_supported_file_type("xml"));
        assert!(is_supported_file_type("html"));

        // Code files
        assert!(is_supported_file_type("rs"));
        assert!(is_supported_file_type("py"));
        assert!(is_supported_file_type("js"));
        assert!(is_supported_file_type("ts"));

        // Rich documents (via xberg)
        assert!(is_supported_file_type("pdf"));
        assert!(is_supported_file_type("docx"));
        assert!(is_supported_file_type("doc"));
        assert!(is_supported_file_type("xlsx"));
        assert!(is_supported_file_type("xls"));
        assert!(is_supported_file_type("pptx"));
        assert!(is_supported_file_type("ppt"));
        assert!(is_supported_file_type("odt"));
        assert!(is_supported_file_type("ods"));
        assert!(is_supported_file_type("odp"));
        assert!(is_supported_file_type("rtf"));
        assert!(is_supported_file_type("epub"));

        // Images (OCR via xberg + Tesseract)
        assert!(is_supported_file_type("png"));
        assert!(is_supported_file_type("jpg"));
        assert!(is_supported_file_type("jpeg"));
        assert!(is_supported_file_type("tiff"));
        assert!(is_supported_file_type("tif"));
        assert!(is_supported_file_type("bmp"));
        assert!(is_supported_file_type("webp"));
        assert!(is_supported_file_type("gif"));

        // Unsupported types
        assert!(!is_supported_file_type("exe"));
        assert!(!is_supported_file_type("bin"));
        assert!(!is_supported_file_type("dll"));
        assert!(!is_supported_file_type("so"));
    }

    #[test]
    fn test_is_plain_text_type() {
        // Plain text types
        assert!(is_plain_text_type("txt"));
        assert!(is_plain_text_type("md"));
        assert!(is_plain_text_type("json"));
        assert!(is_plain_text_type("csv"));
        assert!(is_plain_text_type("xml"));
        assert!(is_plain_text_type("html"));

        // Code files
        assert!(is_plain_text_type("rs"));
        assert!(is_plain_text_type("py"));
        assert!(is_plain_text_type("js"));
        assert!(is_plain_text_type("ts"));
        assert!(is_plain_text_type("yaml"));
        assert!(is_plain_text_type("toml"));

        // Rich documents are NOT plain text
        assert!(!is_plain_text_type("pdf"));
        assert!(!is_plain_text_type("docx"));
        assert!(!is_plain_text_type("doc"));
        assert!(!is_plain_text_type("xlsx"));
        assert!(!is_plain_text_type("pptx"));
        assert!(!is_plain_text_type("epub"));
    }

    #[test]
    fn test_extension_to_mime() {
        // PDF
        assert_eq!(extension_to_mime("pdf"), Some("application/pdf"));

        // Microsoft Office
        assert_eq!(
            extension_to_mime("docx"),
            Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document")
        );
        assert_eq!(extension_to_mime("doc"), Some("application/msword"));
        assert_eq!(
            extension_to_mime("xlsx"),
            Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
        );
        assert_eq!(extension_to_mime("xls"), Some("application/vnd.ms-excel"));
        assert_eq!(
            extension_to_mime("pptx"),
            Some("application/vnd.openxmlformats-officedocument.presentationml.presentation")
        );
        assert_eq!(
            extension_to_mime("ppt"),
            Some("application/vnd.ms-powerpoint")
        );

        // OpenDocument
        assert_eq!(
            extension_to_mime("odt"),
            Some("application/vnd.oasis.opendocument.text")
        );
        assert_eq!(
            extension_to_mime("ods"),
            Some("application/vnd.oasis.opendocument.spreadsheet")
        );
        assert_eq!(
            extension_to_mime("odp"),
            Some("application/vnd.oasis.opendocument.presentation")
        );

        // Other rich formats
        assert_eq!(extension_to_mime("rtf"), Some("application/rtf"));
        assert_eq!(extension_to_mime("epub"), Some("application/epub+zip"));

        // Images (for OCR)
        assert_eq!(extension_to_mime("png"), Some("image/png"));
        assert_eq!(extension_to_mime("jpg"), Some("image/jpeg"));
        assert_eq!(extension_to_mime("jpeg"), Some("image/jpeg"));
        assert_eq!(extension_to_mime("tiff"), Some("image/tiff"));
        assert_eq!(extension_to_mime("tif"), Some("image/tiff"));
        assert_eq!(extension_to_mime("bmp"), Some("image/bmp"));
        assert_eq!(extension_to_mime("webp"), Some("image/webp"));
        assert_eq!(extension_to_mime("gif"), Some("image/gif"));

        // Plain text types should return None (handled differently)
        assert_eq!(extension_to_mime("txt"), None);
        assert_eq!(extension_to_mime("rs"), None);
        assert_eq!(extension_to_mime("json"), None);
    }

    #[tokio::test]
    async fn test_extract_text_valid_utf8() {
        let config = DocumentExtractionConfig::default();
        let data = b"Hello, world!".to_vec();
        let result = extract_text(data, "txt", &config).await.unwrap();
        assert_eq!(result, "Hello, world!");
    }

    #[tokio::test]
    async fn test_extract_text_invalid_utf8() {
        let config = DocumentExtractionConfig::default();
        let data = vec![0xff, 0xfe, 0x00, 0x01];
        let result = extract_text(data, "txt", &config).await;
        assert!(matches!(result, Err(DocumentProcessorError::InvalidUtf8)));
    }

    #[tokio::test]
    async fn test_extract_text_code_file() {
        let config = DocumentExtractionConfig::default();
        let data = b"fn main() {\n    println!(\"Hello\");\n}".to_vec();
        let result = extract_text(data, "rs", &config).await.unwrap();
        assert!(result.contains("fn main()"));
    }

    #[tokio::test]
    async fn test_extract_text_unsupported_extension() {
        // An extension that's not in is_plain_text_type and not in extension_to_mime
        let config = DocumentExtractionConfig::default();
        let data = b"binary data".to_vec();
        let result = extract_text(data, "xyz", &config).await;
        assert!(matches!(
            result,
            Err(DocumentProcessorError::UnsupportedFileType(_))
        ));
    }

    #[test]
    fn test_split_into_sentences() {
        let text = "First sentence. Second sentence! Third sentence?";
        let sentences = split_into_sentences(text);
        assert_eq!(sentences.len(), 3);
        assert_eq!(sentences[0], "First sentence.");
        assert_eq!(sentences[1], "Second sentence!");
        assert_eq!(sentences[2], "Third sentence?");
    }

    #[test]
    fn test_split_into_sentences_with_abbreviations() {
        let text = "Dr. Smith went to the store. He bought milk.";
        let sentences = split_into_sentences(text);
        // Our simple splitter will split on "Dr." but that's acceptable for now
        assert!(!sentences.is_empty());
    }

    #[test]
    fn test_split_into_sentences_no_punctuation() {
        let text = "This is text without sentence-ending punctuation";
        let sentences = split_into_sentences(text);
        assert_eq!(sentences.len(), 1);
        assert_eq!(sentences[0], text);
    }

    #[test]
    fn test_get_last_n_tokens_worth() {
        let tokenizer = create_test_tokenizer();
        let text = "The quick brown fox jumps over the lazy dog";

        // Get last ~5 tokens worth
        let result = get_last_n_tokens_worth(text, 5, &tokenizer);
        assert!(!result.is_empty());
        assert!(tokenizer.encode_with_special_tokens(&result).len() <= 5);
    }

    #[test]
    fn test_get_last_n_tokens_worth_empty() {
        let tokenizer = create_test_tokenizer();
        let result = get_last_n_tokens_worth("", 5, &tokenizer);
        assert!(result.is_empty());
    }

    #[test]
    fn test_count_tokens() {
        let tokenizer = create_test_tokenizer();
        let text = "Hello, world!";
        let count = tokenizer.encode_with_special_tokens(text).len();
        assert!(count > 0);
        assert!(count < 10); // Should be around 4 tokens
    }

    #[test]
    fn test_document_processor_config_default() {
        let config = DocumentProcessorConfig::default();
        assert_eq!(config.max_file_size, 10 * 1024 * 1024);
        assert_eq!(config.max_concurrent_tasks, 4);
        assert_eq!(config.default_max_chunk_tokens, 800);
        assert_eq!(config.default_overlap_tokens, 200);
    }

    #[test]
    fn test_processing_job_serialization() {
        let job = ProcessingJob {
            job_id: Uuid::new_v4(),
            file_id: Uuid::new_v4(),
            vector_store_id: Uuid::new_v4(),
            storage_backend: "database".to_string(),
            storage_path: None,
            chunking_strategy: Some(ChunkingStrategy::Auto),
            callback_url: Some("http://localhost:8080/callback".to_string()),
        };

        let json = serde_json::to_string(&job).unwrap();
        let deserialized: ProcessingJob = serde_json::from_str(&json).unwrap();

        assert_eq!(job.job_id, deserialized.job_id);
        assert_eq!(job.file_id, deserialized.file_id);
        assert_eq!(job.vector_store_id, deserialized.vector_store_id);
        assert_eq!(job.storage_backend, deserialized.storage_backend);
        assert_eq!(job.storage_path, deserialized.storage_path);
        assert_eq!(job.callback_url, deserialized.callback_url);
    }

    /// Integration test for Redis Streams publishing.
    /// Requires Docker - run with `cargo test -- --ignored`
    #[cfg(feature = "redis")]
    #[tokio::test]
    #[ignore = "Requires Docker - run with `cargo test -- --ignored`"]
    async fn test_redis_queue_publishing() {
        use crate::db::tests::harness::redis::create_redis_container;

        let (url, _container) = create_redis_container().await;
        let queue_name = "test_file_processing";
        let consumer_group = "test_workers";

        // Create Redis client and connection
        let client = redis::Client::open(url.as_str()).unwrap();
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();

        // Create consumer group (same as what publish_processing_job does)
        let _: redis::RedisResult<()> = redis::cmd("XGROUP")
            .arg("CREATE")
            .arg(queue_name)
            .arg(consumer_group)
            .arg("0")
            .arg("MKSTREAM")
            .query_async(&mut conn)
            .await;

        // Create a test job
        let job = ProcessingJob {
            job_id: Uuid::new_v4(),
            file_id: Uuid::new_v4(),
            vector_store_id: Uuid::new_v4(),
            storage_backend: "database".to_string(),
            storage_path: None,
            chunking_strategy: Some(ChunkingStrategy::Auto),
            callback_url: None,
        };

        let job_json = serde_json::to_string(&job).unwrap();

        // Publish to stream using XADD (same pattern as publish_processing_job)
        let stream_id: String = redis::cmd("XADD")
            .arg(queue_name)
            .arg("*")
            .arg("job_id")
            .arg(job.job_id.to_string())
            .arg("file_id")
            .arg(job.file_id.to_string())
            .arg("vector_store_id")
            .arg(job.vector_store_id.to_string())
            .arg("data")
            .arg(&job_json)
            .query_async(&mut conn)
            .await
            .unwrap();

        // Verify the job was added
        assert!(!stream_id.is_empty());

        // Read back the job to verify
        let result: redis::Value = redis::cmd("XRANGE")
            .arg(queue_name)
            .arg("-")
            .arg("+")
            .query_async(&mut conn)
            .await
            .unwrap();

        // Verify we got at least one entry
        if let redis::Value::Array(entries) = result {
            assert_eq!(entries.len(), 1);

            // Parse the entry and verify job data
            if let redis::Value::Array(entry) = &entries[0] {
                assert!(entry.len() >= 2);

                // Second element is the field array
                if let redis::Value::Array(fields) = &entry[1] {
                    // Find the "data" field
                    let mut found_data = false;
                    let mut iter = fields.iter();
                    while let (Some(key), Some(val)) = (iter.next(), iter.next()) {
                        if let redis::Value::BulkString(k) = key
                            && k == b"data"
                            && let redis::Value::BulkString(v) = val
                        {
                            let parsed: ProcessingJob = serde_json::from_slice(v).unwrap();
                            assert_eq!(parsed.job_id, job.job_id);
                            assert_eq!(parsed.file_id, job.file_id);
                            assert_eq!(parsed.vector_store_id, job.vector_store_id);
                            found_data = true;
                        }
                    }
                    assert!(found_data, "data field not found in stream entry");
                }
            }
        } else {
            panic!("Expected array from XRANGE");
        }
    }

    // ==========================================================================
    // Integration tests for document extraction (PDF, DOCX, XLSX, RTF)
    // These tests require the test fixtures in tests/fixtures/documents/
    // Run with: cargo test -- --ignored
    // ==========================================================================

    /// Path to test fixtures directory
    const FIXTURES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/documents");

    /// Integration test: Extract text from a plain text file
    #[tokio::test]
    #[ignore = "Integration test - requires test fixtures"]
    #[serial]
    async fn test_extract_text_from_txt_file() {
        let config = DocumentExtractionConfig::default();
        let path = format!("{}/sample.txt", FIXTURES_DIR);
        let data = std::fs::read(&path).expect("Failed to read sample.txt");

        let result = extract_text(data, "txt", &config).await;
        assert!(result.is_ok(), "Failed to extract text: {:?}", result.err());

        let text = result.unwrap();
        assert!(
            text.contains("sample text document"),
            "Expected 'sample text document' in extracted text"
        );
        assert!(
            text.contains("quick brown fox"),
            "Expected 'quick brown fox' in extracted text"
        );
        println!("Extracted {} characters from TXT", text.len());
    }

    /// Integration test: Extract text from a PDF file
    #[cfg(feature = "document-extraction-full")]
    #[tokio::test]
    #[ignore = "Integration test - requires test fixtures and xberg"]
    #[serial]
    async fn test_extract_text_from_pdf_file() {
        let config = DocumentExtractionConfig::default();
        let path = format!("{}/sample.pdf", FIXTURES_DIR);
        let data = std::fs::read(&path).expect("Failed to read sample.pdf");

        let result = extract_text(data, "pdf", &config).await;
        assert!(
            result.is_ok(),
            "Failed to extract text from PDF: {:?}",
            result.err()
        );

        let text = result.unwrap();
        assert!(!text.is_empty(), "Extracted text should not be empty");
        println!(
            "Extracted {} characters from PDF:\n{}",
            text.len(),
            &text[..text.len().min(500)]
        );
    }

    /// Integration test: Extract text from a DOCX file
    #[cfg(feature = "document-extraction-full")]
    #[tokio::test]
    #[ignore = "Integration test - requires test fixtures and xberg"]
    #[serial]
    async fn test_extract_text_from_docx_file() {
        let config = DocumentExtractionConfig::default();
        let path = format!("{}/sample.docx", FIXTURES_DIR);
        let data = std::fs::read(&path).expect("Failed to read sample.docx");

        let result = extract_text(data, "docx", &config).await;
        assert!(
            result.is_ok(),
            "Failed to extract text from DOCX: {:?}",
            result.err()
        );

        let text = result.unwrap();
        assert!(!text.is_empty(), "Extracted text should not be empty");
        assert!(
            text.contains("test DOCX document") || text.contains("integration testing"),
            "Expected test content in extracted text, got: {}",
            &text[..text.len().min(200)]
        );
        println!("Extracted {} characters from DOCX:\n{}", text.len(), text);
    }

    /// Integration test: Extract text from an XLSX file
    #[cfg(feature = "document-extraction-full")]
    #[tokio::test]
    #[ignore = "Integration test - requires test fixtures and xberg"]
    #[serial]
    async fn test_extract_text_from_xlsx_file() {
        let config = DocumentExtractionConfig::default();
        let path = format!("{}/sample.xlsx", FIXTURES_DIR);
        let data = std::fs::read(&path).expect("Failed to read sample.xlsx");

        let result = extract_text(data, "xlsx", &config).await;
        assert!(
            result.is_ok(),
            "Failed to extract text from XLSX: {:?}",
            result.err()
        );

        let text = result.unwrap();
        assert!(!text.is_empty(), "Extracted text should not be empty");
        // XLSX should contain our test data
        assert!(
            text.contains("Name") || text.contains("Test Item"),
            "Expected spreadsheet content in extracted text, got: {}",
            &text[..text.len().min(200)]
        );
        println!("Extracted {} characters from XLSX:\n{}", text.len(), text);
    }

    /// Integration test: Extract text from an RTF file
    #[cfg(feature = "document-extraction-full")]
    #[tokio::test]
    #[ignore = "Integration test - requires test fixtures and xberg"]
    #[serial]
    async fn test_extract_text_from_rtf_file() {
        let config = DocumentExtractionConfig::default();
        let path = format!("{}/sample.rtf", FIXTURES_DIR);
        let data = std::fs::read(&path).expect("Failed to read sample.rtf");

        let result = extract_text(data, "rtf", &config).await;
        assert!(
            result.is_ok(),
            "Failed to extract text from RTF: {:?}",
            result.err()
        );

        let text = result.unwrap();
        assert!(!text.is_empty(), "Extracted text should not be empty");
        assert!(
            text.contains("test RTF document") || text.contains("integration testing"),
            "Expected test content in extracted text, got: {}",
            &text[..text.len().min(200)]
        );
        println!("Extracted {} characters from RTF:\n{}", text.len(), text);
    }

    /// Integration test: Verify corrupted/invalid PDF handling
    #[cfg(feature = "document-extraction-full")]
    #[tokio::test]
    #[ignore = "Integration test - requires xberg"]
    #[serial]
    async fn test_extract_text_from_corrupted_pdf() {
        let config = DocumentExtractionConfig::default();
        // Create invalid PDF data
        let data = b"This is not a valid PDF file, just random bytes".to_vec();

        let result = extract_text(data, "pdf", &config).await;
        assert!(
            result.is_err(),
            "Expected error for corrupted PDF, got: {:?}",
            result.ok()
        );

        if let Err(DocumentProcessorError::DocumentExtraction(msg)) = result {
            println!("Got expected error for corrupted PDF: {}", msg);
        } else {
            panic!("Expected DocumentExtraction error variant");
        }
    }

    /// Integration test: Verify empty file handling
    #[cfg(feature = "document-extraction-full")]
    #[tokio::test]
    #[ignore = "Integration test - requires xberg"]
    #[serial]
    async fn test_extract_text_from_empty_pdf() {
        let config = DocumentExtractionConfig::default();
        let data = Vec::new();

        let result = extract_text(data, "pdf", &config).await;
        // Empty data should produce an error or empty result
        match result {
            Ok(text) => {
                println!("Empty PDF produced empty text (len={})", text.len());
            }
            Err(e) => {
                println!("Empty PDF produced expected error: {}", e);
            }
        }
    }

    /// Integration test: Test extraction with all supported rich document types
    #[cfg(feature = "document-extraction-full")]
    #[tokio::test]
    #[ignore = "Integration test - requires test fixtures and xberg"]
    #[serial]
    async fn test_extract_all_rich_document_types() {
        let config = DocumentExtractionConfig::default();

        let test_cases = [
            ("pdf", "sample.pdf"),
            ("docx", "sample.docx"),
            ("xlsx", "sample.xlsx"),
            ("rtf", "sample.rtf"),
        ];

        for (ext, filename) in test_cases {
            let path = format!("{}/{}", FIXTURES_DIR, filename);
            let data = match std::fs::read(&path) {
                Ok(d) => d,
                Err(e) => {
                    println!("Skipping {} - file not found: {}", filename, e);
                    continue;
                }
            };

            let result = extract_text(data, ext, &config).await;
            match result {
                Ok(text) => {
                    println!(
                        "[{}] Extracted {} characters",
                        ext.to_uppercase(),
                        text.len()
                    );
                    assert!(!text.is_empty(), "{} extraction produced empty text", ext);
                }
                Err(e) => {
                    panic!("[{}] Extraction failed: {}", ext.to_uppercase(), e);
                }
            }
        }
    }

    // ==========================================================================
    // OCR Integration tests (requires Tesseract installed)
    // Run with: cargo test -- --ignored
    // ==========================================================================

    /// Integration test: Extract text from a PNG image using OCR
    #[cfg(feature = "document-extraction-full")]
    #[tokio::test]
    #[ignore = "Integration test - requires Tesseract OCR"]
    #[serial]
    async fn test_extract_text_from_png_with_ocr() {
        let config = DocumentExtractionConfig {
            enable_ocr: true,
            ocr_language: "eng".to_string(),
            ..Default::default()
        };

        let path = format!("{}/sample_text.png", FIXTURES_DIR);
        let data = std::fs::read(&path).expect("Failed to read sample_text.png");

        let result = extract_text(data, "png", &config).await;
        assert!(
            result.is_ok(),
            "Failed to extract text from PNG: {:?}",
            result.err()
        );

        let text = result.unwrap();
        assert!(!text.is_empty(), "OCR should extract text from image");
        println!(
            "Extracted {} characters from PNG via OCR:\n{}",
            text.len(),
            text
        );

        // Check for expected content (case-insensitive due to OCR variations)
        let text_lower = text.to_lowercase();
        assert!(
            text_lower.contains("test")
                || text_lower.contains("image")
                || text_lower.contains("ocr"),
            "OCR should recognize 'test', 'image', or 'ocr' in the text"
        );
    }

    /// Integration test: Extract text from a JPEG image using OCR
    #[cfg(feature = "document-extraction-full")]
    #[tokio::test]
    #[ignore = "Integration test - requires Tesseract OCR"]
    #[serial]
    async fn test_extract_text_from_jpg_with_ocr() {
        let config = DocumentExtractionConfig {
            enable_ocr: true,
            ocr_language: "eng".to_string(),
            ..Default::default()
        };

        let path = format!("{}/sample_text.jpg", FIXTURES_DIR);
        let data = std::fs::read(&path).expect("Failed to read sample_text.jpg");

        let result = extract_text(data, "jpg", &config).await;
        assert!(
            result.is_ok(),
            "Failed to extract text from JPG: {:?}",
            result.err()
        );

        let text = result.unwrap();
        assert!(!text.is_empty(), "OCR should extract text from image");
        println!(
            "Extracted {} characters from JPG via OCR:\n{}",
            text.len(),
            text
        );

        let text_lower = text.to_lowercase();
        assert!(
            text_lower.contains("document")
                || text_lower.contains("processing")
                || text_lower.contains("jpeg"),
            "OCR should recognize content from the JPG image"
        );
    }

    /// Integration test: Extract text from a TIFF image using OCR
    #[cfg(feature = "document-extraction-full")]
    #[tokio::test]
    #[ignore = "Integration test - requires Tesseract OCR"]
    #[serial]
    async fn test_extract_text_from_tiff_with_ocr() {
        let config = DocumentExtractionConfig {
            enable_ocr: true,
            ocr_language: "eng".to_string(),
            ..Default::default()
        };

        let path = format!("{}/sample_invoice.tiff", FIXTURES_DIR);
        let data = std::fs::read(&path).expect("Failed to read sample_invoice.tiff");

        let result = extract_text(data, "tiff", &config).await;
        assert!(
            result.is_ok(),
            "Failed to extract text from TIFF: {:?}",
            result.err()
        );

        let text = result.unwrap();
        assert!(!text.is_empty(), "OCR should extract text from TIFF image");
        println!(
            "Extracted {} characters from TIFF via OCR:\n{}",
            text.len(),
            text
        );

        // Check for expected content
        let text_lower = text.to_lowercase();
        assert!(
            text_lower.contains("test")
                || text_lower.contains("image")
                || text_lower.contains("ocr"),
            "OCR should recognize text content from TIFF image"
        );
    }

    /// Integration test: Verify OCR with different language setting
    #[cfg(feature = "document-extraction-full")]
    #[tokio::test]
    #[ignore = "Integration test - requires Tesseract OCR"]
    #[serial]
    async fn test_ocr_language_configuration() {
        let config = DocumentExtractionConfig {
            enable_ocr: true,
            ocr_language: "eng".to_string(), // English
            ..Default::default()
        };

        let path = format!("{}/sample_text.png", FIXTURES_DIR);
        let data = std::fs::read(&path).expect("Failed to read sample_text.png");

        let result = extract_text(data, "png", &config).await;
        assert!(result.is_ok(), "OCR with 'eng' language should succeed");

        let text = result.unwrap();
        println!("OCR with 'eng' language extracted: {}", text.len());
    }

    /// Integration test: Test all supported image formats with OCR
    #[cfg(feature = "document-extraction-full")]
    #[tokio::test]
    #[ignore = "Integration test - requires Tesseract OCR"]
    #[serial]
    async fn test_ocr_all_image_formats() {
        let config = DocumentExtractionConfig {
            enable_ocr: true,
            ocr_language: "eng".to_string(),
            ..Default::default()
        };

        let test_cases = [
            ("png", "sample_text.png"),
            ("jpg", "sample_text.jpg"),
            ("tiff", "sample_invoice.tiff"),
        ];

        for (ext, filename) in test_cases {
            let path = format!("{}/{}", FIXTURES_DIR, filename);
            let data = match std::fs::read(&path) {
                Ok(d) => d,
                Err(e) => {
                    println!("Skipping {} - file not found: {}", filename, e);
                    continue;
                }
            };

            let result = extract_text(data, ext, &config).await;
            match result {
                Ok(text) => {
                    println!(
                        "[{} OCR] Extracted {} characters",
                        ext.to_uppercase(),
                        text.len()
                    );
                    assert!(
                        !text.is_empty(),
                        "{} OCR extraction produced empty text",
                        ext
                    );
                }
                Err(e) => {
                    panic!("[{} OCR] Extraction failed: {}", ext.to_uppercase(), e);
                }
            }
        }
    }

    /// Integration test: Verify OCR is disabled by default
    #[cfg(feature = "document-extraction-full")]
    #[tokio::test]
    #[ignore = "Integration test - requires xberg"]
    #[serial]
    async fn test_image_extraction_without_ocr_enabled() {
        // With OCR disabled (default), image extraction should still work
        // but may produce empty or minimal text
        let config = DocumentExtractionConfig::default();
        assert!(!config.enable_ocr, "OCR should be disabled by default");

        let path = format!("{}/sample_text.png", FIXTURES_DIR);
        let data = std::fs::read(&path).expect("Failed to read sample_text.png");

        // Without OCR, xberg may return empty text or fail
        let result = extract_text(data, "png", &config).await;
        match result {
            Ok(text) => {
                println!("Image extraction without OCR: {} characters", text.len());
            }
            Err(e) => {
                println!("Image extraction without OCR failed (expected): {}", e);
            }
        }
    }
}
