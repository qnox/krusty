package krusty

import org.gradle.api.GradleException
import java.io.ByteArrayOutputStream
import java.math.BigInteger
import java.security.GeneralSecurityException
import java.security.KeyFactory
import java.security.MessageDigest
import java.security.PublicKey
import java.security.Signature
import java.security.spec.RSAPublicKeySpec
import java.security.spec.X509EncodedKeySpec
import java.util.Base64

/** The maintainer's OpenPGP key, whose primary key certifies the subkey that signs releases. */
private const val RELEASE_ROOT_KEY = "krusty/release-root-key.asc"

/**
 * The trust anchor for releases: an OpenPGP primary key. A release ships its `SHA256SUMS`, a
 * detached signature over it, and the public certificate of the signing subkey; the sums are
 * trusted only when this key bound that subkey for signing, the subkey was valid when it signed,
 * and neither key is revoked for that signature. Rotating the subkey therefore needs no plugin
 * change; replacing the primary key does.
 */
internal class ReleaseRootKey(armoredPublicKey: String) {
    private val bundled = packets(dearmor(armoredPublicKey, "PUBLIC KEY BLOCK"))
    private val root: KeyPacket = bundled.firstOrNull { it.tag == PUBLIC_KEY_TAG }
        ?.let { KeyPacket.parse(it.body) }
        ?: throw GradleException("the krusty release root key is not a supported OpenPGP public key")

    val fingerprint: ByteArray get() = root.fingerprint

    /**
     * Accepts [data] only if [armoredSignature] is a binary-document signature over it by a subkey
     * that [armoredCertificate] binds to this root key for signing, valid when the signature was made
     * and revoked for it neither itself nor through the root key.
     */
    fun verify(data: ByteArray, armoredSignature: String, armoredCertificate: String, what: String) {
        val signature = packets(dearmor(armoredSignature, "SIGNATURE")).singleOrNull { it.tag == SIGNATURE_TAG }
            ?.let { SignaturePacket.parse(it.body, "the signature of $what") }
            ?: throw GradleException("the signature of $what holds no single OpenPGP signature")
        if (signature.type != BINARY_DOCUMENT) {
            throw GradleException("the signature of $what is not a binary-document signature")
        }
        val made = signature.created
            ?: throw GradleException("the signature of $what has no creation time")
        val certificate = packets(dearmor(armoredCertificate, "PUBLIC KEY BLOCK"))
        val primary = certificate.firstOrNull { it.tag == PUBLIC_KEY_TAG }
        if (primary == null || !primary.body.contentEquals(root.body)) {
            throw GradleException("the signing key of $what does not belong to the krusty release root key")
        }
        // The plugin's own copy of the key may carry a revocation as well as the release's copy.
        val keys = certifiedKeys(bundled, "the krusty release root key") + certifiedKeys(certificate, "the signing key of $what")
        if (keys.any { it.subkey == null && it.revokes(made) }) {
            throw GradleException("the krusty release root key is revoked for $what")
        }
        val subkey = keys.firstOrNull { it.subkey != null && it.signs && signature.issuedBy(it.subkey) }
            ?: throw GradleException("$what is not signed by a subkey of the krusty release root key")
        val key = checkNotNull(subkey.subkey)
        if (keys.any { it.subkey != null && it.subkey.body.contentEquals(key.body) && it.revokes(made) }) {
            throw GradleException("the subkey that signed $what is revoked for it")
        }
        if (made < key.created || (subkey.expires != null && made >= subkey.expires)) {
            throw GradleException("$what was signed outside the validity of its signing subkey")
        }
        if (!signature.verifiedBy(key, data)) throw GradleException("the signature of $what does not match it")
    }

    /**
     * What the root key itself says, in a transferable key, about the primary key (`subkey` null)
     * and each subkey: the newest binding of each subkey and every revocation. A signature
     * contributes only when it names the root as issuer and verifies against it, so a packet
     * anyone else made can neither bind nor revoke a key.
     */
    private fun certifiedKeys(packets: List<Packet>, what: String): List<KeyStatement> {
        val statements = mutableListOf<KeyStatement>()
        // Signatures directly after the primary key are about the primary key; a user id or user
        // attribute starts a section whose certifications krusty does not read; a subkey starts its own.
        var section: KeyPacket? = root
        var subkey: KeyPacket? = null
        var newest: SignaturePacket? = null
        fun finish() {
            val key = subkey ?: return
            val binding = newest ?: return
            statements += KeyStatement(
                key,
                signs = binding.keyFlags?.and(SIGN_DATA) == SIGN_DATA,
                expires = binding.keyExpiration?.let { key.created + it },
            )
        }
        for (packet in packets.drop(1)) {
            when (packet.tag) {
                SIGNATURE_TAG -> {
                    val about = section ?: continue
                    val statement = SignaturePacket.parse(packet.body, what)
                    if (!statement.issuedBy(root)) continue
                    val signed = if (about === root) keyHashInput(root.body) else keyHashInput(root.body) + keyHashInput(about.body)
                    when {
                        about === root && statement.type == KEY_REVOCATION ->
                            if (statement.verifiedBy(root, signed)) statements += KeyStatement.revocation(null, statement)
                        about !== root && statement.type == SUBKEY_REVOCATION ->
                            if (statement.verifiedBy(root, signed)) statements += KeyStatement.revocation(about, statement)
                        about !== root && statement.type == SUBKEY_BINDING -> {
                            val created = statement.created ?: continue
                            if (newest?.created?.let { created <= it } == true) continue
                            if (statement.verifiedBy(root, signed)) newest = statement
                        }
                    }
                }
                PUBLIC_SUBKEY_TAG -> {
                    finish()
                    subkey = KeyPacket.parse(packet.body)
                    section = subkey
                    newest = null
                }
                else -> {
                    finish()
                    subkey = null
                    section = null
                    newest = null
                }
            }
        }
        finish()
        return statements
    }

    companion object {
        /** The root key this plugin build ships; a build without one cannot trust any release. */
        fun bundled(): ReleaseRootKey {
            val armored = ReleaseRootKey::class.java.classLoader.getResourceAsStream(RELEASE_ROOT_KEY)
                ?.use { String(it.readBytes(), Charsets.US_ASCII) }
                ?: throw GradleException(
                    "this krusty plugin build carries no release root key; set -Pkrusty.binary=<path> or KRUSTY_BIN",
                )
            return ReleaseRootKey(armored)
        }
    }
}

/**
 * One thing the root key certified about the primary key (`subkey` null) or a subkey: a binding,
 * with its sign-data flag and the epoch second its validity ends, or a revocation from [revokedAt].
 */
private class KeyStatement(
    val subkey: KeyPacket?,
    val signs: Boolean = false,
    val expires: Long? = null,
    private val revokedAt: Long? = null,
    private val hardRevocation: Boolean = false,
) {
    /**
     * Whether this revokes the key for a signature made at [made]. A key retired or superseded
     * (RFC 9580 reasons 1 and 3) keeps the signatures it made before; any other reason, or none,
     * means the key may be compromised and nothing it signed is trusted.
     */
    fun revokes(made: Long): Boolean = revokedAt != null && (hardRevocation || made >= revokedAt)

    companion object {
        fun revocation(subkey: KeyPacket?, signature: SignaturePacket): KeyStatement = KeyStatement(
            subkey,
            revokedAt = signature.created ?: 0,
            hardRevocation = signature.revocationReason != SUPERSEDED && signature.revocationReason != RETIRED,
        )
    }
}

private const val SIGNATURE_TAG = 2
private const val PUBLIC_KEY_TAG = 6
private const val PUBLIC_SUBKEY_TAG = 14

private const val BINARY_DOCUMENT = 0x00
private const val SUBKEY_BINDING = 0x18
private const val KEY_REVOCATION = 0x20
private const val SUBKEY_REVOCATION = 0x28

/** The reasons for revocation that leave the signatures a key made before it in force. */
private const val SUPERSEDED = 1
private const val RETIRED = 3

/** The key flag that lets a key sign data. */
private const val SIGN_DATA = 0x02

private const val RSA = 1
private const val EDDSA_LEGACY = 22
private const val ED25519 = 27

/** The DER object identifier of Ed25519 in a version 4 EdDSA key packet. */
private val ED25519_LEGACY_OID = byteArrayOf(0x2b, 0x06, 0x01, 0x04, 0x01, 0xda.toByte(), 0x47, 0x0f, 0x01)

/** The X.509 SubjectPublicKeyInfo prefix of a raw 32-byte Ed25519 public key. */
private val ED25519_X509_PREFIX = byteArrayOf(
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
)

internal class Packet(val tag: Int, val body: ByteArray)

/** How a version 4 key packet enters a fingerprint or a key signature's hash. */
private fun keyHashInput(body: ByteArray): ByteArray =
    byteArrayOf(0x99.toByte(), (body.size ushr 8).toByte(), body.size.toByte()) + body

/** A version 4 public key or subkey packet of an algorithm releases may be signed with. */
internal class KeyPacket private constructor(
    val body: ByteArray,
    val created: Long,
    val algorithm: Int,
    private val key: PublicKey,
) {
    val fingerprint: ByteArray = MessageDigest.getInstance("SHA-1").digest(keyHashInput(body))

    val keyId: ByteArray get() = fingerprint.copyOfRange(12, 20)

    fun verifies(hash: ByteArray, digestName: String, material: ByteArray): Boolean = when (algorithm) {
        RSA -> Signature.getInstance("NONEwithRSA").run {
            initVerify(key)
            update(digestInfo(digestName) + hash)
            verify(material)
        }
        else -> Signature.getInstance("Ed25519").run {
            initVerify(key)
            update(hash)
            verify(material)
        }
    }

    companion object {
        fun parse(body: ByteArray): KeyPacket? {
            if (body.size < 6 || body[0].toInt() != 4) return null
            val reader = Reader(body, 1)
            val created = reader.u32().toLong() and 0xffffffffL
            val algorithm = reader.byte()
            val key = try {
                when (algorithm) {
                    RSA -> {
                        val modulus = BigInteger(1, reader.mpi())
                        val exponent = BigInteger(1, reader.mpi())
                        KeyFactory.getInstance("RSA").generatePublic(RSAPublicKeySpec(modulus, exponent))
                    }
                    EDDSA_LEGACY -> {
                        val oid = reader.bytes(reader.byte())
                        val point = reader.mpi()
                        if (!oid.contentEquals(ED25519_LEGACY_OID) || point.size != 33 || point[0].toInt() != 0x40) return null
                        ed25519(point.copyOfRange(1, 33))
                    }
                    ED25519 -> ed25519(reader.bytes(32))
                    else -> return null
                }
            } catch (_: IndexOutOfBoundsException) {
                return null
            } catch (_: GeneralSecurityException) {
                // A JVM without Ed25519 (before Java 15) cannot use the key, so nothing it signed is trusted.
                return null
            }
            return KeyPacket(body, created, algorithm, key)
        }

        private fun ed25519(raw: ByteArray): PublicKey =
            KeyFactory.getInstance("Ed25519").generatePublic(X509EncodedKeySpec(ED25519_X509_PREFIX + raw))
    }
}

/** A version 4 signature packet and the hashed subpackets krusty reads. */
internal class SignaturePacket private constructor(
    val type: Int,
    private val algorithm: Int,
    private val digestName: String,
    /** The packet's bytes from its version through its hashed subpackets, as they enter the hash. */
    private val hashed: ByteArray,
    private val issuerFingerprints: List<ByteArray>,
    private val issuerKeyIds: List<ByteArray>,
    private val left16: ByteArray,
    /** The signature value in the form the JCA verifier takes. */
    private val material: ByteArray,
    val created: Long?,
    /** Seconds after the key's creation that a binding signature lets the key expire. */
    val keyExpiration: Long?,
    val keyFlags: Int?,
    /** The reason code of a revocation, when it gives one. */
    val revocationReason: Int?,
) {
    fun issuedBy(key: KeyPacket): Boolean =
        issuerFingerprints.any { it.contentEquals(key.fingerprint) } ||
            (issuerFingerprints.isEmpty() && issuerKeyIds.any { it.contentEquals(key.keyId) })

    /** Whether [key] made this signature over [signed], the bytes before the signature's own hashed part. */
    fun verifiedBy(key: KeyPacket, signed: ByteArray): Boolean {
        if (key.algorithm != algorithm) return false
        val hashedLength = hashed.size
        val hash = MessageDigest.getInstance(digestName).run {
            update(signed)
            update(hashed)
            digest(
                byteArrayOf(
                    4, 0xff.toByte(),
                    (hashedLength ushr 24).toByte(), (hashedLength ushr 16).toByte(),
                    (hashedLength ushr 8).toByte(), hashedLength.toByte(),
                ),
            )
        }
        if (hash[0] != left16[0] || hash[1] != left16[1]) return false
        return try {
            key.verifies(hash, digestName, material)
        } catch (_: GeneralSecurityException) {
            false
        }
    }

    companion object {
        fun parse(body: ByteArray, what: String): SignaturePacket {
            fun malformed(): Nothing = throw GradleException("$what is not a valid OpenPGP signature")
            try {
                if (body[0].toInt() != 4) malformed()
                val type = body[1].toInt() and 0xff
                val algorithm = body[2].toInt() and 0xff
                val digestName = when (body[3].toInt()) {
                    8 -> "SHA-256"
                    9 -> "SHA-384"
                    10 -> "SHA-512"
                    else -> throw GradleException("$what uses a hash krusty does not accept")
                }
                val reader = Reader(body, 4)
                val hashedSubpackets = reader.bytes(reader.u16())
                val hashed = body.copyOfRange(0, reader.offset)
                val unhashedSubpackets = reader.bytes(reader.u16())
                val left16 = reader.bytes(2)
                val material = when (algorithm) {
                    RSA -> reader.mpi()
                    EDDSA_LEGACY -> reader.mpi().leftPad(32, ::malformed) + reader.mpi().leftPad(32, ::malformed)
                    ED25519 -> reader.bytes(64)
                    else -> throw GradleException("$what uses an algorithm krusty does not accept")
                }
                val fingerprints = mutableListOf<ByteArray>()
                val keyIds = mutableListOf<ByteArray>()
                var created: Long? = null
                var keyExpiration: Long? = null
                var keyFlags: Int? = null
                var revocationReason: Int? = null
                subpackets(hashedSubpackets) { subpacket, critical, data ->
                    when (subpacket) {
                        2 -> created = Reader(data, 0).u32().toLong() and 0xffffffffL
                        // A signature that expires is not one a release or its key binding uses.
                        3 -> if (Reader(data, 0).u32() != 0) throw GradleException("$what expires")
                        9 -> keyExpiration = (Reader(data, 0).u32().toLong() and 0xffffffffL).takeIf { it != 0L }
                        16 -> keyIds += data
                        27 -> keyFlags = data[0].toInt() and 0xff
                        29 -> revocationReason = data[0].toInt() and 0xff
                        33 -> if (data.size == 21 && data[0].toInt() == 4) fingerprints += data.copyOfRange(1, 21)
                        // An unknown critical subpacket may restrict the signature in a way this
                        // reader cannot honor, so the signature is rejected rather than trusted.
                        else -> if (critical) throw GradleException("$what has a critical subpacket krusty does not know")
                    }
                }
                // Unhashed subpackets are not signed; an issuer there only helps pick the key.
                subpackets(unhashedSubpackets) { subpacket, _, data -> if (subpacket == 16) keyIds += data }
                return SignaturePacket(
                    type, algorithm, digestName, hashed, fingerprints, keyIds, left16, material,
                    created, keyExpiration, keyFlags, revocationReason,
                )
            } catch (_: IndexOutOfBoundsException) {
                malformed()
            }
        }

        private fun subpackets(area: ByteArray, each: (type: Int, critical: Boolean, data: ByteArray) -> Unit) {
            val reader = Reader(area, 0)
            while (reader.offset < area.size) {
                val first = reader.byte()
                val length = when {
                    first < 192 -> first
                    first < 255 -> ((first - 192) shl 8) + reader.byte() + 192
                    else -> reader.u32()
                }
                if (length < 1) throw IndexOutOfBoundsException()
                val content = reader.bytes(length)
                val type = content[0].toInt() and 0xff
                each(type and 0x7f, type and 0x80 != 0, content.copyOfRange(1, content.size))
            }
        }
    }
}

private fun ByteArray.leftPad(size: Int, malformed: () -> Nothing): ByteArray =
    if (this.size > size) malformed() else ByteArray(size - this.size) + this

/** The DER DigestInfo prefix PKCS #1 v1.5 puts before the hash in an RSA signature. */
private fun digestInfo(digestName: String): ByteArray {
    val hex = when (digestName) {
        "SHA-256" -> "3031300d060960864801650304020105000420"
        "SHA-384" -> "3041300d060960864801650304020205000430"
        else -> "3051300d060960864801650304020305000440"
    }
    return ByteArray(hex.length / 2) { hex.substring(it * 2, it * 2 + 2).toInt(16).toByte() }
}

private class Reader(private val bytes: ByteArray, var offset: Int) {
    fun byte(): Int = bytes[offset++].toInt() and 0xff

    fun u16(): Int = (byte() shl 8) or byte()

    fun u32(): Int = (u16() shl 16) or u16()

    fun bytes(count: Int): ByteArray {
        if (count < 0 || offset + count > bytes.size) throw IndexOutOfBoundsException()
        return bytes.copyOfRange(offset, offset + count).also { offset += count }
    }

    /** An OpenPGP multiprecision integer: a bit count, then the big-endian magnitude. */
    fun mpi(): ByteArray = bytes((u16() + 7) / 8)
}

/** The packets of an OpenPGP message, in either header format; partial lengths are not accepted. */
internal fun packets(data: ByteArray): List<Packet> {
    val packets = mutableListOf<Packet>()
    val reader = Reader(data, 0)
    try {
        while (reader.offset < data.size) {
            val header = reader.byte()
            if (header and 0x80 == 0) throw GradleException("not an OpenPGP packet stream")
            val tag: Int
            val length: Int
            if (header and 0x40 != 0) {
                tag = header and 0x3f
                val first = reader.byte()
                length = when {
                    first < 192 -> first
                    first < 224 -> ((first - 192) shl 8) + reader.byte() + 192
                    first == 255 -> reader.u32()
                    else -> throw GradleException("OpenPGP partial body lengths are not accepted")
                }
            } else {
                tag = (header ushr 2) and 0x0f
                length = when (header and 3) {
                    0 -> reader.byte()
                    1 -> reader.u16()
                    2 -> reader.u32()
                    else -> throw GradleException("OpenPGP indeterminate packet lengths are not accepted")
                }
            }
            packets += Packet(tag, reader.bytes(length))
        }
    } catch (_: IndexOutOfBoundsException) {
        throw GradleException("a truncated OpenPGP packet")
    }
    return packets
}

/** The bytes of an ASCII-armored OpenPGP block of [kind]; the CRC line is not needed to trust them. */
internal fun dearmor(armored: String, kind: String): ByteArray {
    val lines = armored.lines().map { it.trim() }
    val begin = lines.indexOf("-----BEGIN PGP $kind-----")
    val end = lines.indexOf("-----END PGP $kind-----")
    if (begin < 0 || end < begin) throw GradleException("not an armored OpenPGP $kind")
    val body = lines.subList(begin + 1, end)
    // Armor headers end at the first blank line.
    val blank = body.indexOf("")
    val base64 = ByteArrayOutputStream()
    for (line in body.drop(blank + 1)) {
        if (line.startsWith("=")) break
        base64.write(line.toByteArray(Charsets.US_ASCII))
    }
    return try {
        Base64.getDecoder().decode(base64.toByteArray())
    } catch (e: IllegalArgumentException) {
        throw GradleException("not an armored OpenPGP $kind", e)
    }
}
