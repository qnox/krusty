package krusty

import java.security.KeyPair
import java.security.KeyPairGenerator
import java.security.MessageDigest
import java.security.Signature
import java.util.Base64

/**
 * A throwaway Ed25519 root key and signing subkey, so a test can sign the `SHA256SUMS` it
 * generates. Interoperability with GnuPG is covered by the fixtures `ReleaseSignatureTest` reads.
 */
internal class TestReleaseSigner(
    private val subkeyCreated: Long = 1_000_000,
    keyExpiration: Long? = null,
    keyFlags: Int = 0x02,
    /** Makes the subkey sign its own binding while naming the root as issuer, as a forger would. */
    forgeBinding: Boolean = false,
    /** A revocation of the subkey (0x28) the certificate carries. */
    subkeyRevocation: Revocation? = null,
    /** A revocation of the root key (0x20) the certificate carries. */
    rootRevocation: Revocation? = null,
) {
    /** Who makes a revocation: the root, a forger naming the root, or another key naming itself. */
    enum class Issuer { ROOT, FORGED, FOREIGN }

    class Revocation(val time: Long, val reason: Int, val issuer: Issuer = Issuer.ROOT)

    private val rootPair = KeyPairGenerator.getInstance("Ed25519").generateKeyPair()
    private val subkeyPair = KeyPairGenerator.getInstance("Ed25519").generateKeyPair()
    private val foreignPair = KeyPairGenerator.getInstance("Ed25519").generateKeyPair()
    private val root = keyBody(rootPair, 0)
    private val subkey = keyBody(subkeyPair, subkeyCreated)

    val rootKey: ReleaseRootKey = ReleaseRootKey(armor("PUBLIC KEY BLOCK", packet(6, root)))

    /** The subkey's public certificate, as `gpg --armor --export <subkey>!` writes it. */
    val certificate: String = run {
        val subpackets = mutableListOf(created(subkeyCreated), byteArrayOf(2, 27, keyFlags.toByte()))
        if (keyExpiration != null) subpackets += byteArrayOf(5, 9) + u32(keyExpiration)
        subpackets += issuer(root)
        val binding = signature(0x18, if (forgeBinding) subkeyPair else rootPair, hashInput(root) + hashInput(subkey), subpackets)
        val rootSignatures = rootRevocation?.let { packet(2, revocation(0x20, it, hashInput(root))) } ?: ByteArray(0)
        val subkeySignatures = subkeyRevocation
            ?.let { packet(2, revocation(0x28, it, hashInput(root) + hashInput(subkey))) } ?: ByteArray(0)
        armor(
            "PUBLIC KEY BLOCK",
            packet(6, root) + rootSignatures + packet(14, subkey) + packet(2, binding) + subkeySignatures,
        )
    }

    private fun revocation(type: Int, revocation: Revocation, signed: ByteArray): ByteArray {
        val key = when (revocation.issuer) {
            Issuer.ROOT -> rootPair
            Issuer.FORGED -> subkeyPair
            Issuer.FOREIGN -> foreignPair
        }
        val issuer = if (revocation.issuer == Issuer.FOREIGN) issuer(keyBody(foreignPair, 0)) else issuer(root)
        val reason = byteArrayOf(3, 29, revocation.reason.toByte(), 0)
        return signature(type, key, signed, listOf(created(revocation.time), reason, issuer))
    }

    /** An armored binary-document signature over [data] made at [time], as `gpg --detach-sign` writes it. */
    fun sign(data: ByteArray, time: Long = subkeyCreated + 1): String =
        armor("SIGNATURE", packet(2, signature(0x00, subkeyPair, data, listOf(created(time), issuer(subkey)))))

    private fun signature(type: Int, key: KeyPair, signed: ByteArray, subpackets: List<ByteArray>): ByteArray {
        val area = subpackets.reduce(ByteArray::plus)
        val hashed = byteArrayOf(4, type.toByte(), 27, 8, 0, area.size.toByte()) + area
        val hash = MessageDigest.getInstance("SHA-256").run {
            update(signed)
            update(hashed)
            digest(byteArrayOf(4, 0xff.toByte()) + u32(hashed.size.toLong()))
        }
        val value = Signature.getInstance("Ed25519").run {
            initSign(key.private)
            update(hash)
            sign()
        }
        return hashed + byteArrayOf(0, 0) + hash.copyOfRange(0, 2) + value
    }

    private fun keyBody(pair: KeyPair, created: Long): ByteArray =
        byteArrayOf(4) + u32(created) + byteArrayOf(27) + pair.public.encoded.takeLast(32).toByteArray()

    private fun hashInput(body: ByteArray): ByteArray = byteArrayOf(0x99.toByte(), 0, body.size.toByte()) + body

    private fun created(time: Long): ByteArray = byteArrayOf(5, 2) + u32(time)

    private fun issuer(body: ByteArray): ByteArray =
        byteArrayOf(22, 33, 4) + MessageDigest.getInstance("SHA-1").digest(hashInput(body))

    private fun u32(value: Long): ByteArray =
        byteArrayOf((value ushr 24).toByte(), (value ushr 16).toByte(), (value ushr 8).toByte(), value.toByte())

    private fun packet(tag: Int, body: ByteArray): ByteArray {
        check(body.size < 192)
        return byteArrayOf((0xc0 or tag).toByte(), body.size.toByte()) + body
    }

    private fun armor(kind: String, bytes: ByteArray): String =
        "-----BEGIN PGP $kind-----\n\n${Base64.getMimeEncoder().encodeToString(bytes)}\n-----END PGP $kind-----\n"
}
