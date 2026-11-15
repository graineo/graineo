-- Minimal table skeleton of a photo catalog (CREATE TABLE only) for the import tests.
CREATE TABLE AgLibraryRootFolder (
    id_local INTEGER PRIMARY KEY,
    id_global UNIQUE NOT NULL,
    absolutePath UNIQUE NOT NULL DEFAULT '',
    name NOT NULL DEFAULT '',
    relativePathFromCatalog
);
CREATE TABLE AgLibraryFolder (
    id_local INTEGER PRIMARY KEY,
    id_global UNIQUE NOT NULL,
    parentId INTEGER,
    pathFromRoot NOT NULL DEFAULT '',
    rootFolder INTEGER NOT NULL DEFAULT 0,
    visibility INTEGER
);
CREATE TABLE AgLibraryFile (
    id_local INTEGER PRIMARY KEY,
    id_global UNIQUE NOT NULL,
    baseName NOT NULL DEFAULT '',
    errorMessage,
    errorTime,
    extension NOT NULL DEFAULT '',
    externalModTime,
    folder INTEGER NOT NULL DEFAULT 0,
    idx_filename NOT NULL DEFAULT '',
    importHash,
    lc_idx_filename NOT NULL DEFAULT '',
    lc_idx_filenameExtension NOT NULL DEFAULT '',
    md5,
    modTime,
    originalFilename NOT NULL DEFAULT '',
    sidecarExtensions
);
CREATE TABLE Adobe_images (
    id_local INTEGER PRIMARY KEY,
    id_global UNIQUE NOT NULL,
    aspectRatioCache NOT NULL DEFAULT -1,
    bitDepth NOT NULL DEFAULT 0,
    captureTime,
    colorChannels NOT NULL DEFAULT 0,
    colorLabels NOT NULL DEFAULT '',
    colorMode NOT NULL DEFAULT -1,
    copyCreationTime NOT NULL DEFAULT -63113817600,
    copyName,
    copyReason,
    developSettingsIDCache,
    editLock INTEGER NOT NULL DEFAULT 0,
    fileFormat NOT NULL DEFAULT 'unset',
    fileHeight,
    fileWidth,
    hasMissingSidecars INTEGER,
    masterImage INTEGER,
    orientation,
    originalCaptureTime,
    originalRootEntity INTEGER,
    panningDistanceH,
    panningDistanceV,
    pick NOT NULL DEFAULT 0,
    positionInFolder NOT NULL DEFAULT 'z',
    propertiesCache,
    pyramidIDCache,
    rating,
    rootFile INTEGER NOT NULL DEFAULT 0,
    sidecarStatus,
    touchCount NOT NULL DEFAULT 0,
    touchTime NOT NULL DEFAULT 0
);
CREATE TABLE AgHarvestedExifMetadata (
    id_local INTEGER PRIMARY KEY,
    image INTEGER,
    aperture,
    cameraModelRef INTEGER,
    cameraSNRef INTEGER,
    dateDay,
    dateMonth,
    dateYear,
    flashFired INTEGER,
    focalLength,
    gpsLatitude,
    gpsLongitude,
    gpsSequence NOT NULL DEFAULT 0,
    hasGPS INTEGER,
    isoSpeedRating,
    lensRef INTEGER,
    shutterSpeed
);
CREATE TABLE AgInternedExifCameraModel (
    id_local INTEGER PRIMARY KEY,
    searchIndex,
    value
);
CREATE TABLE AgLibraryKeyword (
    id_local INTEGER PRIMARY KEY,
    id_global UNIQUE NOT NULL,
    dateCreated NOT NULL DEFAULT '',
    genealogy NOT NULL DEFAULT '',
    imageCountCache DEFAULT -1,
    includeOnExport INTEGER NOT NULL DEFAULT 1,
    includeParents INTEGER NOT NULL DEFAULT 1,
    includeSynonyms INTEGER NOT NULL DEFAULT 1,
    keywordType,
    lastApplied,
    lc_name,
    name,
    parent INTEGER
);
CREATE TABLE AgLibraryKeywordImage (
    id_local INTEGER PRIMARY KEY,
    image INTEGER NOT NULL DEFAULT 0,
    tag INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE AgLibraryCollection (
    id_local INTEGER PRIMARY KEY,
    creationId NOT NULL DEFAULT '',
    genealogy NOT NULL DEFAULT '',
    imageCount,
    name NOT NULL DEFAULT '',
    parent INTEGER,
    systemOnly NOT NULL DEFAULT ''
);
CREATE TABLE AgLibraryCollectionImage (
    id_local INTEGER PRIMARY KEY,
    collection INTEGER NOT NULL DEFAULT 0,
    image INTEGER NOT NULL DEFAULT 0,
    pick NOT NULL DEFAULT 0,
    positionInCollection
);
CREATE TABLE AgLibraryCollectionContent (
    id_local INTEGER PRIMARY KEY,
    collection INTEGER NOT NULL DEFAULT 0,
    content,
    owningModule
);
CREATE TABLE AgInternedExifLens (
    id_local INTEGER PRIMARY KEY,
    searchIndex,
    value
);
CREATE TABLE AgLibraryIPTC (
    id_local INTEGER PRIMARY KEY,
    altTextAccessibility,
    caption,
    copyright,
    extDescrAccessibility,
    image INTEGER NOT NULL DEFAULT 0
);
