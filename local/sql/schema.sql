-- The message store's schema.
--
-- Implements `spec/12-message-store.md` §2 and
-- `spec/13-message-replay.md` §2.
--
-- Applied to the local SQL container by `ihb-sql`'s tests, and the
-- reference for what the Azure SQL database holds. Every statement is
-- idempotent so a test run does not depend on a clean container.

-- The database the local stack's `.env` files name
-- (`SQL_DATABASE=IntegrationHub`). Those files are the Python's, used
-- unchanged per `spec/18-deployment.md` §5.2, so the database has to be
-- created rather than renamed.
--
-- Nothing else creates it: Azure SQL Edge ships no `sqlcmd` (§5.3.5), so
-- there is no entry-point script, and the tests that apply this schema
-- connected to `master`. Without this the two persistence services start,
-- report **healthy**, and fail every batch with "Cannot open database".
IF DB_ID('IntegrationHub') IS NULL
    EXEC('CREATE DATABASE IntegrationHub');
GO

USE IntegrationHub;
GO

IF SCHEMA_ID('monitoring') IS NULL
    EXEC('CREATE SCHEMA monitoring');
GO

-- `spec/12-message-store.md` §2.
IF OBJECT_ID('monitoring.Message', 'U') IS NULL
CREATE TABLE monitoring.Message (
    Id                  BIGINT IDENTITY(1,1) PRIMARY KEY,
    ReceivedAt          DATETIME2      NOT NULL,
    StoredAt            DATETIME2      NOT NULL,
    CorrelationId       NVARCHAR(200)  NOT NULL,
    SourceSystem        NVARCHAR(200)  NOT NULL,
    ProcessingComponent NVARCHAR(200)  NOT NULL,
    TargetSystem        NVARCHAR(200)  NULL,
    -- §2.3 — the message in full. This is the one place patient data is
    -- stored deliberately.
    RawPayload          NVARCHAR(MAX)  NOT NULL,
    XmlPayload          NVARCHAR(MAX)  NULL,
    SessionId           NVARCHAR(200)  NOT NULL
);
GO

-- `spec/13-message-replay.md` §2.
IF OBJECT_ID('monitoring.MessageReplayQueue', 'U') IS NULL
CREATE TABLE monitoring.MessageReplayQueue (
    ReplayId      BIGINT IDENTITY(1,1) PRIMARY KEY,
    MessageId     BIGINT           NOT NULL,
    ReplayBatchId NVARCHAR(64)     NOT NULL,
    -- §2.1 — exactly 'Pending', 'Failed' or 'Loaded', case-sensitive.
    Status        NVARCHAR(20)     NOT NULL,
    ProcessedAt   DATETIME2        NULL,
    CONSTRAINT FK_MessageReplayQueue_Message
        FOREIGN KEY (MessageId) REFERENCES monitoring.Message (Id)
);
GO

-- The fetch of §3 filters on batch and status, and orders by ReplayId.
IF NOT EXISTS (
    SELECT 1 FROM sys.indexes
    WHERE name = 'IX_MessageReplayQueue_Batch_Status'
)
CREATE INDEX IX_MessageReplayQueue_Batch_Status
    ON monitoring.MessageReplayQueue (ReplayBatchId, Status, ReplayId);
GO
